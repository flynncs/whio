from typing import Annotated

from fastapi import APIRouter, Depends

from app.generated.resolver_v1 import (
    PlaybackResolveRequest,
    PlaybackResolveResponse,
)
from app.youtube.playback import (
    YouTubeMusicPlayback,
)

playback_router = APIRouter(prefix="/v1/playback")


def get_playback() -> YouTubeMusicPlayback:
    return YouTubeMusicPlayback()


@playback_router.post(
    "/resolve",
    response_model=PlaybackResolveResponse,
)
def resolve_playback(
    body: PlaybackResolveRequest,
    playback: Annotated[YouTubeMusicPlayback, Depends(get_playback)],
) -> PlaybackResolveResponse:
    return playback.resolve(body.source)
