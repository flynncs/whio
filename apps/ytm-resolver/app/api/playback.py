from typing import Annotated

from fastapi import APIRouter, Depends, Request
from fastapi.responses import JSONResponse

from app.generated.resolver_v1 import (
    ErrorCode,
    ErrorResponse,
    PlaybackResolveRequest,
    PlaybackResolveResponse,
)
from app.youtube.playback import (
    PlaybackResolutionError,
    UnsupportedProviderError,
    YouTubeMusicPlayback,
)

playback_router = APIRouter(prefix="/v1/playback")


def get_playback() -> YouTubeMusicPlayback:
    return YouTubeMusicPlayback()


@playback_router.post(
    "/resolve",
    response_model=PlaybackResolveResponse,
    responses={422: {"model": ErrorResponse}, 502: {"model": ErrorResponse}},
)
def get_capabilities(
    body: PlaybackResolveRequest,
    request: Request,
    playback: Annotated[YouTubeMusicPlayback, Depends(get_playback)],
) -> PlaybackResolveResponse | JSONResponse:
    try:
        return playback.resolve(body.source)
    except UnsupportedProviderError:
        error = ErrorResponse(
            code=ErrorCode.unsupported_provider,
            message="The provider is not supported",
            request_id=request.state.request_id,
        )
        return JSONResponse(status_code=422, content=error.model_dump(mode="json"))
    except PlaybackResolutionError:
        error = ErrorResponse(
            code=ErrorCode.resolution_failed,
            message="Playback could not be resolved",
            request_id=request.state.request_id,
        )
        return JSONResponse(status_code=502, content=error.model_dump(mode="json"))
