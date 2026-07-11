from collections.abc import Mapping

from app.playback.contracts import (
    PlaybackResolver,
    PlaybackSource,
    ResolvedPlaybackSource,
)


class PlaybackService:
    """Dispatches playback candidates to the matching resolver strategy."""

    def __init__(self, resolvers: Mapping[str, PlaybackResolver]) -> None:
        self._resolvers = dict(resolvers)

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        try:
            resolver = self._resolvers[source.provider]
        except KeyError as error:
            raise LookupError(
                f"No playback resolver registered for {source.provider!r}"
            ) from error

        return await resolver.resolve(source)
