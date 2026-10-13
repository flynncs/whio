import logging
from collections.abc import Mapping
from typing import Any

from requests.exceptions import RequestException
from ytmusicapi import YTMusic
from ytmusicapi.exceptions import YTMusicServerError, YTMusicUserError

from app.errors import ProviderUnavailableError, SourceNotFoundError
from app.youtube.artwork import full_size_artwork_url
from app.generated.resolver_v1 import (
    CatalogueAlbum,
    CatalogueAlbumCandidate,
    CatalogueAlbumSearchResponse,
    CatalogueArtist,
    CatalogueArtistAlbumsResponse,
    CatalogueArtistCandidate,
    CatalogueArtistSearchResponse,
    CatalogueCandidate,
    CatalogueCandidatesResponse,
    CatalogueSearchResponse,
    SourceIdentity,
)


ALBUM_BROWSE_ID_PREFIX = "MPRE"
ATV_VIDEO_TYPE = "MUSIC_VIDEO_TYPE_ATV"
MAX_ALBUM_BROWSE_ID_LENGTH = 128

logger = logging.getLogger("uvicorn.error").getChild(__name__)


class YouTubeMusicCatalogue:
    _client: YTMusic

    def __init__(self, client: YTMusic) -> None:
        self._client = client

    def search(
        self, query: str, limit: int, offset: int = 0
    ) -> CatalogueSearchResponse:
        try:
            results = self._client.search(query, filter="songs", limit=offset + limit)
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        end = offset + limit
        return CatalogueSearchResponse(
            results=[normalize_song_result(result) for result in results[offset:end]],
            next_offset=end if len(results) >= end and end <= 1000 else None,
        )

    def search_albums(
        self, query: str, limit: int, offset: int = 0
    ) -> CatalogueAlbumSearchResponse:
        return self._search_entities(query, limit, offset, "albums")

    def search_artists(
        self, query: str, limit: int, offset: int = 0
    ) -> CatalogueArtistSearchResponse:
        return self._search_entities(query, limit, offset, "artists")

    def _search_entities(
        self, query: str, limit: int, offset: int, kind: str
    ) -> CatalogueAlbumSearchResponse | CatalogueArtistSearchResponse:
        try:
            results = self._client.search(query, filter=kind, limit=offset + limit)
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        end = offset + limit
        page = results[offset:end]
        next_offset = end if len(results) >= end and end <= 1000 else None
        if kind == "albums":
            return CatalogueAlbumSearchResponse(
                results=[candidate for row in page if (candidate := normalize_album_result(row))],
                next_offset=next_offset,
            )
        return CatalogueArtistSearchResponse(
            results=[candidate for row in page if (candidate := normalize_artist_result(row))],
            next_offset=next_offset,
        )

    def fetch_artist(self, source: SourceIdentity) -> CatalogueArtistAlbumsResponse:
        if not source.external_id.startswith("UC"):
            raise SourceNotFoundError("The source is unavailable")
        try:
            artist = self._client.get_artist(source.external_id)
        except YTMusicUserError as error:
            raise SourceNotFoundError("The source is unavailable") from error
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        if not isinstance(artist, Mapping):
            raise SourceNotFoundError("The source is unavailable")
        albums: list[CatalogueAlbumCandidate] = []
        seen: set[str] = set()
        for shelf in (artist.get("albums"), artist.get("singles")):
            if not isinstance(shelf, Mapping):
                continue
            rows = shelf.get("results")
            params = shelf.get("params")
            browse_id = shelf.get("browseId")
            if params and browse_id:
                try:
                    expanded = self._client.get_artist_albums(browse_id, params)
                except YTMusicUserError as error:
                    raise SourceNotFoundError("The source is unavailable") from error
                except (YTMusicServerError, RequestException) as error:
                    raise ProviderUnavailableError(
                        "The provider is temporarily unavailable"
                    ) from error
                if isinstance(expanded, list):
                    rows = expanded
                elif isinstance(expanded, Mapping):
                    rows = expanded.get("results", [])
                else:
                    raise ProviderUnavailableError(
                        "The provider returned an invalid artist shelf"
                    )
            if not isinstance(rows, list):
                continue
            for row in rows[:100]:
                candidate = normalize_album_result(row)
                if candidate is None or candidate.source.external_id in seen:
                    continue
                seen.add(candidate.source.external_id)
                albums.append(candidate)
                if len(albums) == 200:
                    return CatalogueArtistAlbumsResponse(results=albums)
        return CatalogueArtistAlbumsResponse(results=albums)

    def fetch_album(self, source: SourceIdentity) -> CatalogueCandidatesResponse:
        if not is_album_browse_id(source.external_id):
            raise SourceNotFoundError("The source is unavailable")
        try:
            album = self._client.get_album(source.external_id)
        except YTMusicUserError as error:
            raise SourceNotFoundError("The source is unavailable") from error
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        artwork_url = parse_artwork_url(album.get("thumbnails"))
        album_identity = CatalogueAlbum(
            title=album["title"],
            source=source,
            year=parse_year(album.get("year")),
            release_type=parse_release_type(album.get("type")),
            artwork_url=artwork_url,
        )
        album_artists = album.get("artists")
        tracks = album.get("tracks", [])
        audio_tracks = self._album_audio_tracks(album, source.external_id)
        pairs = pair_album_audio_tracks(tracks, audio_tracks)

        return CatalogueCandidatesResponse(
            results=[
                normalize_album_track(
                    track, album_identity, album_artists, artwork_url, pairs.get(position)
                )
                for position, track in enumerate(tracks)
            ],
            reported_count=parse_count(album.get("trackCount")),
        )

    def _album_audio_tracks(
        self, album: Mapping[str, Any], album_id: str
    ) -> list[Any]:
        playlist_id = album.get("audioPlaylistId")
        if not isinstance(playlist_id, str) or not playlist_id:
            return []
        try:
            playlist = self._client.get_playlist(playlist_id, limit=None)
        except (YTMusicUserError, YTMusicServerError, RequestException) as error:
            logger.warning(
                "album audio playlist unavailable album_id=%s error_type=%s",
                album_id,
                type(error).__name__,
            )
            return []
        tracks = playlist.get("tracks") if isinstance(playlist, Mapping) else None
        return tracks if isinstance(tracks, list) else []


def normalize_album_result(
    result: Mapping[str, Any],
) -> CatalogueAlbumCandidate | None:
    if not isinstance(result, Mapping):
        return None
    external_id = result.get("browseId")
    title = result.get("title")
    artists = result.get("artists")
    if (
        not isinstance(external_id, str)
        or not external_id.startswith(ALBUM_BROWSE_ID_PREFIX)
        or len(external_id) > 512
        or not isinstance(title, str)
        or not title.strip()
        or len(title) > 500
        or not isinstance(artists, list)
    ):
        return None
    named_artists = [
        CatalogueArtist(name=artist["name"], source=parse_source(artist.get("id")))
        for artist in artists
        if isinstance(artist, Mapping)
        and isinstance(artist.get("name"), str)
        and artist["name"].strip()
        and len(artist["name"]) <= 500
    ]
    if not named_artists:
        return None
    return CatalogueAlbumCandidate(
        source=SourceIdentity(provider_id="youtube_music", external_id=external_id),
        title=title,
        artists=named_artists,
        year=nonnegative_year(result.get("year")),
        release_type=bounded_release_type(result.get("type")),
        artwork_url=parse_artwork_url(result.get("thumbnails")),
    )


def normalize_artist_result(
    result: Mapping[str, Any],
) -> CatalogueArtistCandidate | None:
    if not isinstance(result, Mapping):
        return None
    external_id = result.get("browseId")
    name = result.get("artist") or result.get("name")
    if (
        not isinstance(external_id, str)
        or not external_id.startswith("UC")
        or len(external_id) > 512
        or not isinstance(name, str)
        or not name.strip()
        or len(name) > 500
    ):
        return None
    return CatalogueArtistCandidate(
        source=SourceIdentity(provider_id="youtube_music", external_id=external_id),
        name=name,
    )


def normalize_song_result(result: Mapping[str, Any]) -> CatalogueCandidate:
    return CatalogueCandidate(
        source=SourceIdentity(
            provider_id="youtube_music", external_id=result["videoId"]
        ),
        title=result["title"],
        artists=[parse_artist(artist) for artist in result.get("artists", [])],
        duration_ms=parse_duration_ms(result.get("duration")),
        album=parse_album(result.get("album")),
        artwork_url=parse_artwork_url(result.get("thumbnails")),
    )


def normalize_album_track(
    track: Mapping[str, Any],
    album: CatalogueAlbum,
    album_artists: Any,
    artwork_url: str | None,
    audio: Mapping[str, Any] | None = None,
) -> CatalogueCandidate:
    artists = track.get("artists") or album_artists or []
    video_id = track["videoId"]
    source_id = video_id
    alternate_sources: list[SourceIdentity] = []
    duration_ms = parse_duration_seconds(track.get("duration_seconds"))
    if audio is not None and track.get("videoType") != ATV_VIDEO_TYPE:
        audio_id = audio.get("videoId")
        if isinstance(audio_id, str) and audio_id and audio_id != video_id:
            source_id = audio_id
            alternate_sources = [
                SourceIdentity(provider_id="youtube_music", external_id=video_id)
            ]
            duration_ms = parse_duration_seconds(audio.get("duration_seconds"))

    return CatalogueCandidate(
        source=SourceIdentity(provider_id="youtube_music", external_id=source_id),
        title=track["title"],
        artists=[parse_artist(artist) for artist in artists],
        duration_ms=duration_ms,
        album=album,
        artwork_url=artwork_url,
        track_number=parse_positive_number(track.get("trackNumber")),
        disc_number=parse_positive_number(track.get("discNumber")),
        alternate_sources=alternate_sources,
    )


def pair_album_audio_tracks(
    tracks: list[Any],
    audio_tracks: list[Any],
) -> dict[int, Any]:
    if not tracks or not audio_tracks:
        return {}
    if len(tracks) == len(audio_tracks) and all(
        normalize_track_title(track.get("title"))
        == normalize_track_title(audio.get("title"))
        for track, audio in zip(tracks, audio_tracks, strict=True)
    ):
        return dict(enumerate(audio_tracks))

    positions_by_title = _normalized_title_positions(audio_tracks)
    album_positions = _normalized_title_positions(tracks)

    pairs: dict[int, Any] = {}
    for title, positions in album_positions.items():
        audio_positions = positions_by_title.get(title, [])
        if len(positions) == 1 and len(audio_positions) == 1:
            pairs[positions[0]] = audio_tracks[audio_positions[0]]
    return pairs


def _normalized_title_positions(rows: list[Any]) -> dict[str, list[int]]:
    positions: dict[str, list[int]] = {}
    for position, row in enumerate(rows):
        title = normalize_track_title(row.get("title"))
        if title:
            positions.setdefault(title, []).append(position)
    return positions


def is_album_browse_id(value: str) -> bool:
    return (
        value.startswith(ALBUM_BROWSE_ID_PREFIX)
        and len(value) <= MAX_ALBUM_BROWSE_ID_LENGTH
        and all(
            character.isascii() and (character.isalnum() or character in "_-")
            for character in value
        )
    )


def normalize_track_title(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return " ".join(value.casefold().split())


def parse_artist(value: Mapping[str, Any]) -> CatalogueArtist:
    return CatalogueArtist(name=value["name"], source=parse_source(value.get("id")))


def parse_album(value: Any) -> CatalogueAlbum | None:
    if isinstance(value, Mapping):
        title = value.get("name")
        if isinstance(title, str) and title.strip():
            return CatalogueAlbum(
                title=title.strip(), source=parse_source(value.get("id"))
            )
    return None


def parse_source(external_id: Any) -> SourceIdentity | None:
    if isinstance(external_id, str) and external_id.strip() and len(external_id) <= 512:
        return SourceIdentity(
            provider_id="youtube_music", external_id=external_id.strip()
        )
    return None


def nonnegative_year(value: Any) -> int | None:
    year = parse_year(value)
    return year if year is not None and year >= 0 else None


def bounded_release_type(value: Any) -> str | None:
    release_type = parse_release_type(value)
    return release_type if release_type is not None and len(release_type) <= 100 else None


def parse_year(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    if isinstance(value, str) and value.strip().isdigit():
        return int(value.strip())
    return None


def parse_release_type(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


def parse_duration_seconds(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value * 1000
    return None


def parse_positive_number(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 1:
        return value
    return None


def parse_count(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value
    return None


def parse_artwork_url(thumbnails: Any) -> str | None:
    if not isinstance(thumbnails, list):
        return None

    for thumbnail in reversed(thumbnails):
        if isinstance(thumbnail, Mapping):
            url = thumbnail.get("url")
            if isinstance(url, str) and url:
                return full_size_artwork_url(url)
    return None


def parse_duration_ms(value: str | None) -> int | None:
    if not value:
        return None

    parts = value.strip().split(":")
    if len(parts) not in (2, 3):
        return None

    try:
        numbers = [int(part) for part in parts]
    except ValueError:
        return None

    if len(numbers) == 2:
        minutes, seconds = numbers
        return (minutes * 60 + seconds) * 1000

    hours, minutes, seconds = numbers
    return (hours * 3600 + minutes * 60 + seconds) * 1000
