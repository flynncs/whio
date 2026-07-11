from collections.abc import Mapping
from typing import ClassVar


class LumenError(Exception):
    """Base class for expected, application-level failures."""

    code: ClassVar[str] = "lumen_error"
    public_message: ClassVar[str] = "An application error occurred"

    def __init__(
        self,
        *,
        context: Mapping[str, str] | None = None,
    ) -> None:
        super().__init__(self.public_message)
        self.context = dict(context or {})
