from typing import Protocol

from app.catalogue.domain import ProviderId
from app.playback.domain import PlaybackSource, ResolvedPlaybackSource


class PlaybackResolver(Protocol):
    provider: ProviderId

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource: ...
