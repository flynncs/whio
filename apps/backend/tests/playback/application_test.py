import unittest
from uuid import UUID

from app.catalogue.domain import ProviderId, SourceIdentity, Track
from app.errors import NoPlayableSourceError, TrackNotFoundError
from app.playback.application import ResolveTrackPlayback
from app.playback.domain import ResolvedPlaybackSource


TRACK_ID = UUID("00000000-0000-0000-0000-000000000001")


class FakeTrackRepository:
    def __init__(
        self,
        track: Track | None,
        sources: tuple[SourceIdentity, ...] = (),
    ) -> None:
        self.track = track
        self.sources = sources

    def get_track(self, track_id: UUID) -> Track | None:
        return (
            self.track if self.track is not None and self.track.id == track_id else None
        )

    def get_sources(self, track_id: UUID) -> tuple[SourceIdentity, ...]:
        return (
            self.sources if self.track is not None and self.track.id == track_id else ()
        )


class FakePlaybackGateway:
    def __init__(self) -> None:
        self.sources: list[SourceIdentity] = []

    async def resolve(self, source: SourceIdentity) -> ResolvedPlaybackSource:
        self.sources.append(source)
        return ResolvedPlaybackSource(
            provider=source.provider,
            external_id=source.external_id,
            url="https://example.test/audio",
            headers={},
        )


def make_track() -> Track:
    return Track(
        id=TRACK_ID,
        title="Instant Crush",
        artists=("Daft Punk",),
        duration_seconds=337,
    )


class ResolveTrackPlaybackTests(unittest.IsolatedAsyncioTestCase):
    async def test_resolves_the_tracks_first_catalogue_source(self) -> None:
        first = SourceIdentity(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
        )
        second = SourceIdentity(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
        )
        gateway = FakePlaybackGateway()
        use_case = ResolveTrackPlayback(
            tracks=FakeTrackRepository(make_track(), (first, second)),
            playback=gateway,
        )

        resolved = await use_case.execute(TRACK_ID)

        self.assertEqual(gateway.sources, [first])
        self.assertEqual(resolved.external_id, "abc123")

    async def test_rejects_an_unknown_track(self) -> None:
        use_case = ResolveTrackPlayback(
            tracks=FakeTrackRepository(None),
            playback=FakePlaybackGateway(),
        )

        with self.assertRaises(TrackNotFoundError):
            await use_case.execute(TRACK_ID)

    async def test_rejects_a_track_without_sources(self) -> None:
        use_case = ResolveTrackPlayback(
            tracks=FakeTrackRepository(make_track()),
            playback=FakePlaybackGateway(),
        )

        with self.assertRaises(NoPlayableSourceError):
            await use_case.execute(TRACK_ID)


if __name__ == "__main__":
    unittest.main()
