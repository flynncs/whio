from collections.abc import Mapping
from dataclasses import dataclass

from app.catalogue.domain import ProviderId


@dataclass(frozen=True, slots=True)
class PlaybackSource:
    """A provider-bound candidate that can be resolved for playback."""

    provider: ProviderId
    source_type: str
    external_id: str


@dataclass(frozen=True, slots=True)
class ResolvedPlaybackSource:
    """A short-lived source ready for the generic streaming layer."""

    provider: ProviderId
    external_id: str
    url: str
    headers: Mapping[str, str]
    codec: str | None = None
    bitrate: float | None = None
    duration: float | None = None
