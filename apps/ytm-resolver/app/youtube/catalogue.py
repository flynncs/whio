from collections.abc import Mapping
from typing import Any

from requests.exceptions import RequestException
from ytmusicapi import YTMusic
from ytmusicapi.exceptions import YTMusicServerError

from app.errors import ProviderUnavailableError
from app.generated.resolver_v1 import (
    CatalogueAlbum,
    CatalogueArtist,
    CatalogueCandidate,
    CatalogueSearchResponse,
    SourceIdentity,
)


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
