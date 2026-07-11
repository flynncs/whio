from collections.abc import Mapping
from dataclasses import dataclass
from typing import Protocol


@dataclass(frozen=True, slots=True)
class PlaybackSource:
    """A provider-bound candidate that can be resolved for playback."""

    provider: str
    source_type: str
    external_id: str


@dataclass(frozen=True, slots=True)
class ResolvedPlaybackSource:
    """A short-lived source ready for the generic streaming layer."""

    provider: str
    external_id: str
    url: str
    headers: Mapping[str, str]
    codec: str | None = None
    bitrate: float | None = None
    duration: float | None = None


class PlaybackResolver(Protocol):
    """Strategy interface implemented by playback-capable integrations."""

    provider: str

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        """Resolve one candidate into a short-lived playable source."""

        ...
