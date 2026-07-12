import unittest
from uuid import UUID

from app.domain.catalogue import CatalogueSearchResult, SourceIdentity
from app.domain.catalogue_repository import InMemoryCatalogueRepository
from app.domain.providers import ProviderName
from app.services.track_identity import TrackIdentityService


class TrackIdentityServiceTests(unittest.TestCase):
    def test_resolves_candidate_to_track_and_attaches_source(self) -> None:
        repository = InMemoryCatalogueRepository()
        expected_id = UUID("00000000-0000-0000-0000-000000000001")
        candidate = CatalogueSearchResult(
            provider=ProviderName.YOUTUBE_MUSIC,
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
                    provider=ProviderName.YOUTUBE_MUSIC,
                    external_id="abc123",
                ),
            ),
        )

    def test_reuses_existing_track_without_generating_another_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        expected_id = UUID("00000000-0000-0000-0000-000000000001")
        candidate = CatalogueSearchResult(
            provider=ProviderName.YOUTUBE_MUSIC,
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
