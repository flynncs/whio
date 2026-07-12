import unittest
from uuid import UUID

from app.catalogue.domain import ProviderId, SourceIdentity, Track
from app.errors import SourceConflictError, TrackNotFoundError
from app.persistence.in_memory.catalogue_repository import InMemoryCatalogueRepository


class CatalogueRepositoryTests(unittest.TestCase):
    def _add_track(
        self,
        repository: InMemoryCatalogueRepository,
        track_id: str = "00000000-0000-0000-0000-000000000001",
    ) -> Track:
        track = Track(
            id=UUID(track_id),
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        repository.add_track(track)
        return track

    def test_track_and_source_can_be_loaded_by_lumen_id(self) -> None:
        repository = InMemoryCatalogueRepository()
        track = self._add_track(repository)
        source = SourceIdentity(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
        )

        repository.attach_source(track.id, source)

        self.assertEqual(repository.get_track(track.id), track)
        self.assertEqual(repository.find_track_by_source(source), track)
        self.assertEqual(repository.get_sources(track.id), (source,))

    def test_can_attach_an_additional_source_to_a_track(self) -> None:
        repository = InMemoryCatalogueRepository()
        track = self._add_track(repository)
        primary_source = SourceIdentity(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
        )
        alternate_source = SourceIdentity(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
            upstream_server_id=UUID("00000000-0000-0000-0000-000000000001"),
        )

        repository.attach_source(track.id, primary_source)
        repository.attach_source(track.id, alternate_source)

        self.assertEqual(
            repository.get_sources(track.id),
            (primary_source, alternate_source),
        )

    def test_attaching_the_same_source_twice_is_idempotent(self) -> None:
        repository = InMemoryCatalogueRepository()
        track = self._add_track(repository)
        source = SourceIdentity(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
        )

        repository.attach_source(track.id, source)
        repository.attach_source(track.id, source)

        self.assertEqual(repository.get_sources(track.id), (source,))

    def test_cannot_attach_one_source_to_two_tracks(self) -> None:
        repository = InMemoryCatalogueRepository()
        first = self._add_track(repository)
        second = self._add_track(
            repository,
            track_id="00000000-0000-0000-0000-000000000002",
        )
        source = SourceIdentity(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
        )

        repository.attach_source(first.id, source)

        with self.assertRaises(SourceConflictError):
            repository.attach_source(second.id, source)

    def test_cannot_attach_source_to_unknown_track(self) -> None:
        repository = InMemoryCatalogueRepository()

        with self.assertRaises(TrackNotFoundError):
            repository.attach_source(
                UUID("00000000-0000-0000-0000-000000000099"),
                SourceIdentity(
                    provider=ProviderId.NAVIDROME,
                    external_id="song-456",
                ),
            )


if __name__ == "__main__":
    unittest.main()
