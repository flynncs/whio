import unittest
from uuid import UUID

from app.domain.catalogue import CatalogueSearchResult, SourceIdentity
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

    def test_recording_and_source_can_be_loaded_by_lumen_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        candidate = CatalogueSearchResult(
            provider="youtube_music",
            external_id="abc123",
            title="Instant Crush",
            artists=["Daft Punk"],
            duration_seconds=337,
        )

        imported = repository.import_candidate(candidate)

        self.assertEqual(repository.get_recording(imported.id), imported)

        sources = repository.get_sources(imported.id)

        self.assertEqual(len(sources), 1)
        self.assertEqual(sources[0].recording_id, imported.id)
        self.assertEqual(
            sources[0].identity,
            SourceIdentity(
                provider=candidate.provider,
                external_id=candidate.external_id,
            ),
        )

    def test_can_attach_an_additional_source_to_a_recording(self) -> None:
        repository = InMemoryCatalogueRepository()
        candidate = CatalogueSearchResult(
            provider="youtube_music",
            external_id="abc123",
            title="Instant Crush",
            artists=["Daft Punk"],
            duration_seconds=337,
        )
        imported = repository.import_candidate(candidate)
        alternate_source = SourceIdentity(
            provider="navidrome",
            external_id="song-456",
            upstream_server_id=UUID("00000000-0000-0000-0000-000000000001"),
        )

        repository.attach_source(imported.id, alternate_source)

        sources = repository.get_sources(imported.id)
        self.assertEqual(len(sources), 2)
        self.assertEqual(sources[1].recording_id, imported.id)
        self.assertEqual(sources[1].identity, alternate_source)

    def test_attaching_the_same_source_twice_is_idempotent(self) -> None:
        repository = InMemoryCatalogueRepository()
        candidate = CatalogueSearchResult(
            provider="youtube_music",
            external_id="abc123",
            title="Instant Crush",
            artists=["Daft Punk"],
            duration_seconds=337,
        )
        imported = repository.import_candidate(candidate)
        source = SourceIdentity(
            provider="navidrome",
            external_id="song-456",
        )

        repository.attach_source(imported.id, source)
        repository.attach_source(imported.id, source)

        self.assertEqual(len(repository.get_sources(imported.id)), 2)

    def test_cannot_attach_one_source_to_two_recordings(self) -> None:
        repository = InMemoryCatalogueRepository()
        first = repository.import_candidate(
            CatalogueSearchResult(
                provider="youtube_music",
                external_id="abc123",
                title="Instant Crush",
                artists=["Daft Punk"],
            )
        )
        second = repository.import_candidate(
            CatalogueSearchResult(
                provider="youtube_music",
                external_id="def456",
                title="Get Lucky",
                artists=["Daft Punk"],
            )
        )
        source = SourceIdentity(
            provider="navidrome",
            external_id="song-456",
        )

        repository.attach_source(first.id, source)

        with self.assertRaises(ValueError):
            repository.attach_source(second.id, source)

    def test_cannot_attach_source_to_unknown_recording(self) -> None:
        repository = InMemoryCatalogueRepository()

        with self.assertRaises(LookupError):
            repository.attach_source(
                UUID("00000000-0000-0000-0000-000000000099"),
                SourceIdentity(
                    provider="navidrome",
                    external_id="song-456",
                ),
            )


if __name__ == "__main__":
    unittest.main()
