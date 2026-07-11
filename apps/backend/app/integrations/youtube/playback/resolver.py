import json
import subprocess

from pydantic import BaseModel, Field


class ResolvedYouTubeAudio(BaseModel):
    url: str
    headers: dict[str, str] = Field(default_factory=dict)
    codec: str | None = None
    bitrate: float | None = None
    duration: float | None = None


def resolve_audio(video_id: str) -> ResolvedYouTubeAudio:
    command = [
        "yt-dlp",
        "--ignore-config",
        "--no-playlist",
        "--format",
        "bestaudio[acodec=opus]/bestaudio",
        "--dump-single-json",
        f"https://www.youtube.com/watch?v={video_id}",
    ]

    result = subprocess.run(
        command, capture_output=True, text=True, check=True, timeout=30
    )

    stream = json.loads(result.stdout)

    return ResolvedYouTubeAudio(
        url=stream["url"],
        headers=stream.get("http_headers", {}),
        codec=stream.get("acodec"),
        bitrate=stream.get("abr"),
        duration=stream.get("duration"),
    )
