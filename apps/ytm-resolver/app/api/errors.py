from fastapi import Request
from fastapi.exceptions import RequestValidationError
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


async def request_validation_exception_handler(
    request: Request, exc: Exception
) -> JSONResponse:
    assert isinstance(exc, RequestValidationError)

    error = ErrorResponse(
        code=ErrorCode.invalid_request,
        message="The request is invalid",
        request_id=request.state.request_id,
    )

    return JSONResponse(status_code=400, content=error.model_dump(mode="json"))


async def unexpected_exception_handler(
    request: Request, exc: Exception
) -> JSONResponse:
    request_id = request.state.request_id

    error = ErrorResponse(
        code=ErrorCode.internal_error,
        message="An unexpected error occurred",
        request_id=request_id,
    )

    return JSONResponse(
        status_code=500,
        content=error.model_dump(mode="json"),
        headers={"X-Request-ID": request_id},
    )
