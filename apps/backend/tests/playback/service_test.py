import unittest

from app.domain.providers import ProviderName
from app.playback.contracts import (
    PlaybackSource,
    ResolvedPlaybackSource,
)
from app.playback.service import PlaybackService


class FakePlaybackResolver:
    provider = ProviderName.YOUTUBE_MUSIC

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        return ResolvedPlaybackSource(
            provider=source.provider,
            external_id=source.external_id,
            url="https://example.test/audio",
            headers={},
        )


class PlaybackServiceTests(unittest.IsolatedAsyncioTestCase):
    async def test_dispatches_to_resolver_for_source_provider(self) -> None:
        service = PlaybackService(
            resolvers={ProviderName.YOUTUBE_MUSIC: FakePlaybackResolver()}
        )
        source = PlaybackSource(
            provider=ProviderName.YOUTUBE_MUSIC,
            source_type="temporary",
            external_id="abc123",
        )

        resolved = await service.resolve(source)

        self.assertEqual(resolved.provider, ProviderName.YOUTUBE_MUSIC)
        self.assertEqual(resolved.external_id, "abc123")

    async def test_rejects_unregistered_provider(self) -> None:
        service = PlaybackService(resolvers={})
        source = PlaybackSource(
            provider=ProviderName.YOUTUBE_MUSIC,
            source_type="temporary",
            external_id="abc123",
        )

        with self.assertRaises(LookupError):
            await service.resolve(source)


if __name__ == "__main__":
    unittest.main()
