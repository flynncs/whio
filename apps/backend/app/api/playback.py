from fastapi import APIRouter, Request

from app.integrations.youtube.playback.stream import stream_youtube_audio

router = APIRouter(prefix="/playback", tags=["Playback"])


@router.get("/stream/{video_id}")
async def stream(video_id: str, request: Request):
    return await stream_youtube_audio(
        video_id, range_header=request.headers.get("range")
    )
