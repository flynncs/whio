import asyncio
import json
import subprocess

from app.catalogue.domain import ProviderId
from app.errors import ResolverProviderMismatchError
from app.playback.domain import PlaybackSource, ResolvedPlaybackSource
from app.playback.ports import PlaybackResolver
from app.providers.youtube.models import YtDlpAudioPayload


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
        provider=ProviderId.YOUTUBE_MUSIC,
        external_id=video_id,
        url=payload.url,
        headers=payload.http_headers,
        codec=payload.acodec,
        bitrate=payload.abr,
        duration=payload.duration,
    )


class YouTubePlaybackResolver(PlaybackResolver):
    """YouTube implementation of the generic playback resolver strategy."""

    provider = ProviderId.YOUTUBE_MUSIC

    async def resolve(self, source: PlaybackSource) -> ResolvedPlaybackSource:
        if source.provider != self.provider:
            raise ResolverProviderMismatchError(
                expected=self.provider, actual=source.provider
            )

        return await asyncio.to_thread(_resolve_audio, source.external_id)
