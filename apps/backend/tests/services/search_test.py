import unittest

from app.domain.catalogue import CatalogueSearchResult, SourceIdentity
from app.domain.catalogue_repository import InMemoryCatalogueRepository
from app.services.search import SearchService


class FakeCatalogueSearchProvider:
    def __init__(
        self,
        provider: str,
        candidates: list[CatalogueSearchResult],
    ) -> None:
        self.provider = provider
        self._candidates = candidates
        self.calls: list[tuple[str, int]] = []

    def search(self, query: str, limit: int) -> list[CatalogueSearchResult]:
        self.calls.append((query, limit))
        return list(self._candidates)


class SearchServiceTests(unittest.TestCase):
    def test_search_provisions_tracks_and_attaches_provider_sources(self) -> None:
        repository = InMemoryCatalogueRepository()
        youtube_provider = FakeCatalogueSearchProvider(
            provider="youtube_music",
            candidates=[
                CatalogueSearchResult(
                    provider="youtube_music",
                    external_id="abc123",
                    title="Instant Crush",
                    artists=("Daft Punk",),
                    duration_seconds=337,
                )
            ],
        )
        navidrome_provider = FakeCatalogueSearchProvider(
            provider="navidrome",
            candidates=[
                CatalogueSearchResult(
                    provider="navidrome",
                    external_id="song-456",
                    title="Instant Crush",
                    artists=("Daft Punk",),
                    duration_seconds=337,
                )
            ],
        )
        service = SearchService(
            repository=repository,
            providers=(youtube_provider, navidrome_provider),
        )

        first_results = service.search("Daft Punk", limit=5)
        second_results = service.search("Daft Punk", limit=5)

        self.assertEqual(len(first_results), 2)
        self.assertEqual(
            [track.id for track in first_results],
            [track.id for track in second_results],
        )
        self.assertEqual(
            repository.get_sources(first_results[0].id),
            (SourceIdentity(provider="youtube_music", external_id="abc123"),),
        )
        self.assertEqual(
            repository.get_sources(first_results[1].id),
            (SourceIdentity(provider="navidrome", external_id="song-456"),),
        )
        self.assertEqual(youtube_provider.calls, [("Daft Punk", 5)] * 2)
        self.assertEqual(navidrome_provider.calls, [("Daft Punk", 5)] * 2)


if __name__ == "__main__":
    unittest.main()
