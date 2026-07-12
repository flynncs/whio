from app.errors.base import LumenError
from app.domain.providers import ProviderName


class ResolverProviderMismatchError(LumenError):
    code = "resolver_provider_mismatch"
    public_message = "Playback resolver configuration error"

    def __init__(self, expected: ProviderName, actual: ProviderName) -> None:
        super().__init__(
            context={
                "expected_provider": expected.value,
                "actual_provider": actual.value,
            }
        )
