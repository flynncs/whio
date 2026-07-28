import subprocess

from pydantic import AnyUrl, BaseModel, ConfigDict, Field

from app.generated.resolver_v1 import (
    MediaMetadata,
    PlaybackResolveResponse,
    SourceIdentity,
)


class UnsupportedProviderError(Exception):
    pass


class PlaybackResolutionError(Exception):
    pass


class YtDlpAudioPayload(BaseModel):
    model_config = ConfigDict(extra="ignore")

    url: AnyUrl
    http_headers: dict[str, str] = Field(default_factory=dict)
    acodec: str | None = None
    abr: float | None = None
    duration: float | None = None


def _resolve_audio(video_id: str) -> PlaybackResolveResponse:
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
    payload = YtDlpAudioPayload.model_validate_json(result.stdout)
    duration_ms = (
        round(payload.duration * 1000) if payload.duration is not None else None
    )
    return PlaybackResolveResponse(
        url=payload.url,
        headers=payload.http_headers,
        media=MediaMetadata(
            codec=payload.acodec,
            bitrate_kbps=payload.abr,
            duration_ms=duration_ms,
            content_type=None,
            content_length_bytes=None,
        ),
    )


class YouTubeMusicPlayback:
    def resolve(
        self,
        source: SourceIdentity,
    ) -> PlaybackResolveResponse:
        if source.provider_id != "youtube_music":
            raise UnsupportedProviderError(source.provider_id)

        try:
            return _resolve_audio(source.external_id)
        except subprocess.CalledProcessError as error:
            raise PlaybackResolutionError("Playback could not be resolved") from error
