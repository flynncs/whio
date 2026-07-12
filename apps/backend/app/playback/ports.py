from typing import Protocol

from app.catalogue.domain import SourceIdentity
from app.playback.domain import ResolvedPlaybackSource


class PlaybackGateway(Protocol):
    async def resolve(self, source: SourceIdentity) -> ResolvedPlaybackSource: ...
