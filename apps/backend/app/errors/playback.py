from app.errors.base import LumenError


class ResolverProviderMismatchError(LumenError):
    code = "resolver_provider_mismatch"
    public_message = "Playback resolver configuration error"

    def __init__(self, expected: str, actual: str) -> None:
        super().__init__(
            context={
                "expected_provider": expected,
                "actual_provider": actual,
            }
        )
