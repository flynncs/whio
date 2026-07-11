from collections.abc import Mapping
from typing import Any

from ytmusicapi import YTMusic

from app.domain.catalogue import CatalogueSearchResult

ytmusic = YTMusic()


def search_songs(query: str, limit: int) -> list[CatalogueSearchResult]:
    results = ytmusic.search(query, filter="songs", limit=limit)

    return [normalize_song_result(result) for result in results]


def normalize_song_result(result: Mapping[str, Any]) -> CatalogueSearchResult:
    return CatalogueSearchResult(
        provider="youtube_music",
        external_id=result["videoId"],
        title=result["title"],
        artists=[artist["name"] for artist in result.get("artists", [])],
        duration_seconds=parse_duration(result.get("duration")),
    )


def parse_duration(value: str | None) -> int | None:
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
        return minutes * 60 + seconds

    hours, minutes, seconds = numbers
    return hours * 3600 + minutes * 60 + seconds
