import unittest

from app.catalogue.domain import ProviderId
from app.playback.domain import PlaybackSource, ResolvedPlaybackSource
from app.providers.gateway import ProviderPlaybackGateway


class FakePlaybackResolver:
    provider = ProviderId.YOUTUBE_MUSIC

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        return ResolvedPlaybackSource(
            provider=source.provider,
            external_id=source.external_id,
            url="https://example.test/audio",
            headers={},
        )


class ProviderPlaybackGatewayTests(unittest.IsolatedAsyncioTestCase):
    async def test_dispatches_to_resolver_for_source_provider(self) -> None:
        service = ProviderPlaybackGateway(
            resolvers={ProviderId.YOUTUBE_MUSIC: FakePlaybackResolver()}
        )
        source = PlaybackSource(
            provider=ProviderId.YOUTUBE_MUSIC,
            source_type="temporary",
            external_id="abc123",
        )

        resolved = await service.resolve(source)

        self.assertEqual(resolved.provider, ProviderId.YOUTUBE_MUSIC)
        self.assertEqual(resolved.external_id, "abc123")

    async def test_rejects_unregistered_provider(self) -> None:
        service = ProviderPlaybackGateway(resolvers={})
        source = PlaybackSource(
            provider=ProviderId.YOUTUBE_MUSIC,
            source_type="temporary",
            external_id="abc123",
        )

        with self.assertRaises(LookupError):
            await service.resolve(source)


if __name__ == "__main__":
    unittest.main()
