from fastapi import APIRouter, Request

from app.integrations.youtube.playback.resolver import YouTubePlaybackResolver
from app.playback.contracts import PlaybackSource
from app.playback.service import PlaybackService
from app.playback.stream_session import stream_source

router = APIRouter(prefix="/playback", tags=["Playback"])

playback_service = PlaybackService(
    resolvers={"youtube_music": YouTubePlaybackResolver()}
)


@router.get("/stream/{video_id}")
async def stream(video_id: str, request: Request):
    source = PlaybackSource(
        provider="youtube_music",
        source_type="youtube_music",
        external_id=video_id,
    )
    resolved = await playback_service.resolve(source)

    return await stream_source(
        resolved,
        range_header=request.headers.get("range"),
    )
