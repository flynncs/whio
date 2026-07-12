import asyncio
import json
import subprocess

from app.errors import ResolverProviderMismatchError
from app.domain.providers import ProviderName
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
        provider=ProviderName.YOUTUBE_MUSIC,
        external_id=video_id,
        url=payload.url,
        headers=payload.http_headers,
        codec=payload.acodec,
        bitrate=payload.abr,
        duration=payload.duration,
    )


class YouTubePlaybackResolver(PlaybackResolver):
    """YouTube implementation of the generic playback resolver strategy."""

    provider = ProviderName.YOUTUBE_MUSIC

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        if source.provider != self.provider:
            raise ResolverProviderMismatchError(
                expected=self.provider, actual=source.provider
            )

        return await asyncio.to_thread(_resolve_audio, source.external_id)
