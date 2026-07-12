from typing import Annotated
from uuid import UUID

from fastapi import APIRouter, Depends, Request

from app.delivery.http.dependencies import get_resolve_track_playback
from app.delivery.http.streaming import stream_source
from app.playback.application import ResolveTrackPlayback

router = APIRouter(prefix="/playback", tags=["Playback"])


@router.get("/stream/{track_id}")
async def stream(
    track_id: UUID,
    request: Request,
    playback: Annotated[
        ResolveTrackPlayback,
        Depends(get_resolve_track_playback),
    ],
):
    resolved = await playback.execute(track_id)

    return await stream_source(
        resolved,
        range_header=request.headers.get("range"),
    )
