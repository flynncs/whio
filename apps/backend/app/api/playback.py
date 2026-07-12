from typing import Annotated

from fastapi import APIRouter, Depends, Request

from app.api.dependencies import get_playback_service
from app.playback.contracts import PlaybackSource
from app.playback.service import PlaybackService
from app.playback.stream_session import stream_source

router = APIRouter(prefix="/playback", tags=["Playback"])


@router.get("/stream/{video_id}")
async def stream(
    video_id: str,
    request: Request,
    service: Annotated[PlaybackService, Depends(get_playback_service)],
):
    source = PlaybackSource(
        provider="youtube_music",
        source_type="youtube_music",
        external_id=video_id,
    )
    resolved = await service.resolve(source)

    return await stream_source(
        resolved,
        range_header=request.headers.get("range"),
    )
