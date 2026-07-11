import unittest

from app.playback.contracts import (
    PlaybackSource,
    ResolvedPlaybackSource,
)
from app.playback.service import PlaybackService


class FakePlaybackResolver:
    provider = "fake"

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        return ResolvedPlaybackSource(
            provider=source.provider,
            external_id=source.external_id,
            url="https://example.test/audio",
            headers={},
        )


class PlaybackServiceTests(unittest.IsolatedAsyncioTestCase):
    async def test_dispatches_to_resolver_for_source_provider(self) -> None:
        service = PlaybackService(resolvers={"fake": FakePlaybackResolver()})
        source = PlaybackSource(
            provider="fake",
            source_type="temporary",
            external_id="abc123",
        )

        resolved = await service.resolve(source)

        self.assertEqual(resolved.provider, "fake")
        self.assertEqual(resolved.external_id, "abc123")

    async def test_rejects_unregistered_provider(self) -> None:
        service = PlaybackService(resolvers={})
        source = PlaybackSource(
            provider="unknown",
            source_type="temporary",
            external_id="abc123",
        )

        with self.assertRaises(LookupError):
            await service.resolve(source)


if __name__ == "__main__":
    unittest.main()
