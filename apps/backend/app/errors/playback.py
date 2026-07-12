from app.errors.base import LumenError
from app.catalogue.domain import ProviderId


class ResolverProviderMismatchError(LumenError):
    code = "resolver_provider_mismatch"
    public_message = "Playback resolver configuration error"

    def __init__(self, expected: ProviderId, actual: ProviderId) -> None:
        super().__init__(
            context={
                "expected_provider": expected.value,
                "actual_provider": actual.value,
            }
        )
