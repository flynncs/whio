import asyncio
import json
import subprocess

from app.catalogue.domain import ProviderId, SourceIdentity
from app.errors import PlaybackProviderMismatchError
from app.integrations.youtube_music.models import YtDlpAudioPayload
from app.playback.domain import ResolvedPlaybackSource


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
        command,
        capture_output=True,
        text=True,
        check=True,
        timeout=30,
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


class YouTubeMusicPlayback:
    """Resolves a YouTube Music source into temporary playable media."""

    async def resolve(
        self,
        source: SourceIdentity,
    ) -> ResolvedPlaybackSource:
        if source.provider != ProviderId.YOUTUBE_MUSIC:
            raise PlaybackProviderMismatchError(
                expected=ProviderId.YOUTUBE_MUSIC,
                actual=source.provider,
            )

        return await asyncio.to_thread(_resolve_audio, source.external_id)
