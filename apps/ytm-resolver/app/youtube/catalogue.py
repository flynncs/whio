from collections.abc import Mapping
from typing import Any

from requests.exceptions import RequestException
from ytmusicapi import YTMusic
from ytmusicapi.exceptions import YTMusicServerError

from app.errors import ProviderUnavailableError
from app.generated.resolver_v1 import Artist, CatalogueCandidate, SourceIdentity


class YouTubeMusicCatalogue:
    _client: YTMusic

    def __init__(self, client: YTMusic) -> None:
        self._client = client

    def search(self, query: str, limit: int) -> list[CatalogueCandidate]:
        try:
            results = self._client.search(query, filter="songs", limit=limit)
        except (YTMusicServerError, RequestException) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
        return [normalize_song_result(result) for result in results]


def normalize_song_result(result: Mapping[str, Any]) -> CatalogueCandidate:
    return CatalogueCandidate(
        source=SourceIdentity(
            provider_id="youtube_music", external_id=result["videoId"]
        ),
        title=result["title"],
        artists=[Artist(artist["name"]) for artist in result.get("artists", [])],
        duration_ms=parse_duration_ms(result.get("duration")),
    )


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
