from fastapi import Request
from fastapi.responses import JSONResponse

from app.errors import (
    PlaybackResolutionError,
    ProviderUnavailableError,
    ResolverError,
    UnsupportedProviderError,
)
from app.generated.resolver_v1 import ErrorCode, ErrorResponse

ERROR_RESPONSES: dict[
    type[ResolverError],
    tuple[int, ErrorCode, str],
] = {
    UnsupportedProviderError: (
        422,
        ErrorCode.unsupported_provider,
        "The provider is not supported",
    ),
    PlaybackResolutionError: (
        502,
        ErrorCode.resolution_failed,
        "Playback could not be resolved",
    ),
    ProviderUnavailableError: (
        503,
        ErrorCode.provider_unavailable,
        "The provider is temporarily unavailable",
    ),
}


async def resolver_exception_handler(request: Request, exc: Exception) -> JSONResponse:
    assert isinstance(exc, ResolverError)

    status, code, message = ERROR_RESPONSES.get(
        type(exc),
        (
            500,
            ErrorCode.internal_error,
            "An unexpected error occurred",
        ),
    )

    error = ErrorResponse(
        code=code,
        message=message,
        request_id=request.state.request_id,
    )

    return JSONResponse(
        status_code=status,
        content=error.model_dump(mode="json"),
    )
