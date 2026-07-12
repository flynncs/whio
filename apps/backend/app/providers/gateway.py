from collections.abc import Mapping

from app.catalogue.domain import ProviderId
from app.playback.domain import PlaybackSource, ResolvedPlaybackSource
from app.playback.ports import PlaybackResolver


class ProviderPlaybackGateway:
    """Dispatches provider-bound playback sources to provider adapters."""

    def __init__(
        self,
        resolvers: Mapping[ProviderId, PlaybackResolver],
    ) -> None:
        self._resolvers = dict(resolvers)

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        try:
            resolver = self._resolvers[source.provider]
        except KeyError as error:
            raise LookupError(
                f"No playback resolver registered for {source.provider!r}"
            ) from error

        return await resolver.resolve(source)
