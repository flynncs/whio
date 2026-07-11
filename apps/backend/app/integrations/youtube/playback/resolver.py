import asyncio
import json
import subprocess

from app.integrations.youtube.playback.models import YtDlpAudioPayload
from app.playback.contracts import (
    PlaybackResolver,
    PlaybackSource,
    ResolvedPlaybackSource,
)


def _resolve_audio(video_id: str) -> ResolvedPlaybackSource:
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

    payload = YtDlpAudioPayload.model_validate(json.loads(result.stdout))

    return ResolvedPlaybackSource(
        provider="youtube_music",
        external_id=video_id,
        url=payload.url,
        headers=payload.http_headers,
        codec=payload.acodec,
        bitrate=payload.abr,
        duration=payload.duration,
    )


class YouTubePlaybackResolver(PlaybackResolver):
    """YouTube implementation of the generic playback resolver strategy."""

    provider = "youtube_music"

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        if source.provider != self.provider:
            raise ValueError(
                f"Expected provider {self.provider!r}, got {source.provider!r}"
            )

        return await asyncio.to_thread(_resolve_audio, source.external_id)
