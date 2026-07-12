from collections.abc import Sequence
from typing import Protocol

from app.domain.catalogue import (
    CatalogueSearchResult,
    Track,
)
from app.domain.providers import ProviderName
from app.services.track_identity import TrackIdentityResolver


class CatalogueSearchProvider(Protocol):
    provider: ProviderName

    def search(self, query: str, limit: int) -> list[CatalogueSearchResult]: ...


class SearchService:
    def __init__(
        self,
        identity_resolver: TrackIdentityResolver,
        providers: Sequence[CatalogueSearchProvider],
    ) -> None:
        self._identity_resolver = identity_resolver
        self._providers = tuple(providers)

    def search(self, query: str, limit: int) -> list[Track]:
        tracks: list[Track] = []

        for provider in self._providers:
            candidates = provider.search(query, limit)
            for candidate in candidates:
                tracks.append(self._identity_resolver.resolve_candidate(candidate))

        return tracks
