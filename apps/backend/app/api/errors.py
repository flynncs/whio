import logging

from fastapi import Request
from fastapi.responses import JSONResponse

from app.errors import LumenError

logger = logging.getLogger(__name__)

STATUS_BY_ERROR_CODE = {
    "track_not_found": 404,
    "source_conflict": 409,
    "provider_unavailable": 503,
    "search_unavailable": 503,
    "resolver_provider_mismatch": 500,
}


def _request_id(request: Request) -> str | None:
    return getattr(request.state, "request_id", None)


async def lumen_error_handler(
    request: Request,
    exception: Exception,
) -> JSONResponse:
    if not isinstance(exception, LumenError):
        return await unexpected_error_handler(request, exception)

    request_id = _request_id(request)
    content = {
        "code": exception.code,
        "message": exception.public_message,
    }

    if request_id is not None:
        content["request_id"] = request_id

    return JSONResponse(
        status_code=STATUS_BY_ERROR_CODE.get(exception.code, 500),
        content=content,
    )


async def unexpected_error_handler(
    request: Request,
    exception: Exception,
) -> JSONResponse:
    request_id = _request_id(request)
    logger.exception(
        "Unhandled exception while processing request",
        extra={
            "request_id": request_id,
            "path": request.url.path,
        },
    )

    content = {
        "code": "internal_error",
        "message": "Internal server error",
    }

    if request_id is not None:
        content["request_id"] = request_id

    return JSONResponse(status_code=500, content=content)
