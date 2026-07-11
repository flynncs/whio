import unittest

from app.domain.catalogue import CatalogueSearchResult
from app.domain.catalogue_repository import InMemoryCatalogueRepository


class CatalogueRepositoryTests(unittest.TestCase):
    def test_same_external_source_keeps_same_lumen_track_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        candidate = CatalogueSearchResult(
            provider="youtube_music",
            external_id="abc123",
            title="Instant Crush",
            artists=["Daft Punk"],
            duration_seconds=337,
        )

        first = repository.import_candidate(candidate)
        second = repository.import_candidate(candidate)

        self.assertEqual(first.id, second.id)

    def test_track_can_be_loaded_by_lumen_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        candidate = CatalogueSearchResult(
            provider="youtube_music",
            external_id="abc123",
            title="Instant Crush",
            artists=["Daft Punk"],
            duration_seconds=337,
        )

        imported = repository.import_candidate(candidate)

        sources = repository.get_sources(imported.id)

        self.assertEqual(len(sources), 1)
        self.assertEqual(sources[0].recording_id, imported.id)
        self.assertEqual(sources[0].provider, candidate.provider)
        self.assertEqual(sources[0].external_id, candidate.external_id)


if __name__ == "__main__":
    unittest.main()
