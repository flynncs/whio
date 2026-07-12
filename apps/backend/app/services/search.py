from collections.abc import Sequence
from typing import Protocol
from uuid import uuid7

from app.domain.catalogue import (
    CatalogueSearchResult,
    SourceIdentity,
    Track,
)
from app.domain.catalogue_repository import CatalogueRepository


class CatalogueSearchProvider(Protocol):
    provider: str

    def search(self, query: str, limit: int) -> list[CatalogueSearchResult]: ...


class SearchService:
    def __init__(
        self,
        repository: CatalogueRepository,
        providers: Sequence[CatalogueSearchProvider],
    ) -> None:
        self._repository = repository
        self._providers = tuple(providers)

    def search(self, query: str, limit: int) -> list[Track]:
        tracks: list[Track] = []

        for provider in self._providers:
            candidates = provider.search(query, limit)
            for candidate in candidates:
                tracks.append(self._import_candidate(candidate))

        return tracks

    def _import_candidate(self, candidate: CatalogueSearchResult) -> Track:
        source_identity = SourceIdentity(
            provider=candidate.provider,
            external_id=candidate.external_id,
        )

        existing_track = self._repository.find_track_by_source(source=source_identity)

        if existing_track is not None:
            return existing_track

        track_id = uuid7()

        track = Track(
            id=track_id,
            title=candidate.title,
            artists=tuple(candidate.artists),
            duration_seconds=candidate.duration_seconds,
            provisional=True,
        )

        self._repository.add_track(track)
        self._repository.attach_source(track_id, source=source_identity)
        return track
