from typing import Protocol

from app.domain.catalogue import CatalogueSearchResult, Recording
from app.domain.catalogue_repository import CatalogueRepository


class CatalogueSearchProvider(Protocol):
    provider: str

    def search(self, query: str, limit: int) -> list[CatalogueSearchResult]: ...


class SearchService:
    def __init__(
        self,
        repository: CatalogueRepository,
        providers: list[CatalogueSearchProvider],
    ) -> None:
        self._repository = repository
        self._providers = providers

    def search(self, query: str, limit: int) -> list[Recording]:
        recordings: list[Recording] = []

        for provider in self._providers:
            candidates = provider.search(query, limit)
            for candidate in candidates:
                recordings.append(self._repository.import_candidate(candidate))

        return recordings
