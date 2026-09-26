from collections.abc import Mapping
from typing import Any

from requests.exceptions import RequestException
from ytmusicapi import YTMusic
from ytmusicapi.exceptions import YTMusicServerError, YTMusicUserError

from app.errors import ProviderUnavailableError, SourceNotFoundError
from app.generated.resolver_v1 import (
    CatalogueAlbum,
    CatalogueArtist,
    CatalogueCandidate,
    CatalogueCandidatesResponse,
    CatalogueSearchResponse,
    SourceIdentity,
)


ALBUM_BROWSE_ID_PREFIX = "MPRE"


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

    def fetch_album(self, source: SourceIdentity) -> CatalogueCandidatesResponse:
        if not source.external_id.startswith(ALBUM_BROWSE_ID_PREFIX):
            raise SourceNotFoundError("The source is unavailable")
        try:
            album = self._client.get_album(source.external_id)
        except YTMusicUserError as error:
            raise SourceNotFoundError("The source is unavailable") from error
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        album_identity = CatalogueAlbum(
            title=album["title"],
            source=source,
            year=parse_year(album.get("year")),
            release_type=parse_release_type(album.get("type")),
        )
        artwork_url = parse_artwork_url(album.get("thumbnails"))
        album_artists = album.get("artists")

        return CatalogueCandidatesResponse(
            results=[
                normalize_album_track(track, album_identity, album_artists, artwork_url)
                for track in album.get("tracks", [])
            ],
            reported_count=parse_count(album.get("trackCount")),
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
) -> CatalogueCandidate:
    artists = track.get("artists") or album_artists or []

    return CatalogueCandidate(
        source=SourceIdentity(
            provider_id="youtube_music", external_id=track["videoId"]
        ),
        title=track["title"],
        artists=[parse_artist(artist) for artist in artists],
        duration_ms=parse_duration_seconds(track.get("duration_seconds")),
        album=album,
        artwork_url=artwork_url,
    )


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
    if isinstance(external_id, str) and external_id.strip():
        return SourceIdentity(
            provider_id="youtube_music", external_id=external_id.strip()
        )
    return None


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


def parse_count(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
        return value
    return None


def parse_artwork_url(thumbnails: Any) -> str | None:
    """the largest thumbnail is the last entry in ytmusicapi's ordering."""
    if not isinstance(thumbnails, list):
        return None

    for thumbnail in reversed(thumbnails):
        if isinstance(thumbnail, Mapping):
            url = thumbnail.get("url")
            if isinstance(url, str) and url:
                return url
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
