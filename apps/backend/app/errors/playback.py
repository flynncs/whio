from app.catalogue.domain import ProviderId
from app.errors.base import LumenError


class PlaybackProviderMismatchError(LumenError):
    code = "playback_provider_mismatch"
    public_message = "Playback provider configuration error"

    def __init__(self, expected: ProviderId, actual: ProviderId) -> None:
        super().__init__(
            context={
                "expected_provider": expected.value,
                "actual_provider": actual.value,
            }
        )


class NoPlayableSourceError(LumenError):
    code = "no_playable_source"
    public_message = "No playable source found for the track"
