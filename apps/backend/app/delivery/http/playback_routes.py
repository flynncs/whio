from typing import Annotated

from fastapi import APIRouter, Depends, Request

from app.catalogue.domain import ProviderId
from app.delivery.http.dependencies import get_playback_gateway
from app.playback.domain import PlaybackSource
from app.providers.gateway import ProviderPlaybackGateway
from app.delivery.http.streaming import stream_source

router = APIRouter(prefix="/playback", tags=["Playback"])


@router.get("/stream/{video_id}")
async def stream(
    video_id: str,
    request: Request,
    service: Annotated[
        ProviderPlaybackGateway,
        Depends(get_playback_gateway),
    ],
):
    source = PlaybackSource(
        provider=ProviderId.YOUTUBE_MUSIC,
        source_type="youtube_music",
        external_id=video_id,
    )
    resolved = await service.resolve(source)

    return await stream_source(
        resolved,
        range_header=request.headers.get("range"),
    )
