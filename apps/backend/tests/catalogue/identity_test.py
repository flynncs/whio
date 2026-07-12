import unittest
from uuid import UUID

from app.catalogue.application import TrackIdentityService
from app.catalogue.domain import CatalogueResult, ProviderId, SourceIdentity
from app.persistence.in_memory.catalogue_repository import InMemoryCatalogueRepository


class TrackIdentityServiceTests(unittest.TestCase):
    def test_resolves_candidate_to_track_and_attaches_source(self) -> None:
        repository = InMemoryCatalogueRepository()
        expected_id = UUID("00000000-0000-0000-0000-000000000001")
        candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        service = TrackIdentityService(
            repository=repository,
            id_factory=lambda: expected_id,
        )

        track = service.resolve_candidate(candidate)

        self.assertEqual(track.id, expected_id)
        self.assertEqual(track.title, "Instant Crush")
        self.assertEqual(track.artists, ("Daft Punk",))
        self.assertEqual(repository.get_track(expected_id), track)
        self.assertEqual(
            repository.get_sources(expected_id),
            (
                SourceIdentity(
                    provider=ProviderId.YOUTUBE_MUSIC,
                    external_id="abc123",
                ),
            ),
        )

    def test_reuses_existing_track_without_generating_another_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        expected_id = UUID("00000000-0000-0000-0000-000000000001")
        candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        ids = iter((expected_id,))
        service = TrackIdentityService(
            repository=repository,
            id_factory=lambda: next(ids),
        )

        first = service.resolve_candidate(candidate)
        second = service.resolve_candidate(candidate)

        self.assertEqual(first, second)
        self.assertEqual(first.id, expected_id)


if __name__ == "__main__":
    unittest.main()
