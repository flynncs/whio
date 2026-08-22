import hashlib
import logging
import subprocess
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from urllib.parse import parse_qs, urlsplit

import requests
from pydantic import AnyUrl, BaseModel, ConfigDict, Field

from app.errors import (
    PlaybackResolutionError,
    ProviderUnavailableError,
    UnsupportedProviderError,
)
from app.generated.resolver_v1 import (
    MediaMetadata,
    PlaybackResolveResponse,
    SourceIdentity,
)

logger = logging.getLogger("uvicorn.error").getChild(__name__)

_AUDIO_FORMATS = (
    "bestaudio[acodec=opus]",
    "bestaudio[ext=m4a]",
)
_RESOLUTION_PASSES = 2
_RETRY_DELAY_SECONDS = 0.2


class YtDlpAudioPayload(BaseModel):
    model_config = ConfigDict(extra="ignore")

    url: AnyUrl
    http_headers: dict[str, str] = Field(default_factory=dict)
    acodec: str | None = None
    abr: float | None = None
    duration: float | None = None
    format_id: str | None = None
    ext: str | None = None


def _extract_audio(
    video_id: str,
    audio_format: str,
) -> YtDlpAudioPayload:
    command = [
        "yt-dlp",
        "--ignore-config",
        "--no-playlist",
        "--format",
        audio_format,
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
    return YtDlpAudioPayload.model_validate_json(result.stdout)


def _stream_status(payload: YtDlpAudioPayload) -> int:
    headers = {**payload.http_headers, "Range": "bytes=0-"}

    with requests.get(
        str(payload.url),
        headers=headers,
        stream=True,
        allow_redirects=True,
        timeout=(5, 10),
    ) as response:
        return response.status_code


def _playback_response(payload: YtDlpAudioPayload) -> PlaybackResolveResponse:
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


def _is_retryable_status(status_code: int) -> bool:
    return status_code in (403, 429) or status_code >= 500


def _media_url_log_fields(
    payload: YtDlpAudioPayload,
) -> tuple[str, str, str, str, str]:
    url = str(payload.url)
    parsed = urlsplit(url)
    query = parse_qs(parsed.query)

    return (
        parsed.hostname or "unknown",
        query.get("c", ["unknown"])[0],
        query.get("mn", ["unknown"])[0],
        query.get("expire", ["unknown"])[0],
        hashlib.sha256(url.encode()).hexdigest()[:12],
    )


def _resolve_candidate(
    video_id: str,
    audio_format: str,
) -> tuple[YtDlpAudioPayload, int]:
    payload = _extract_audio(video_id, audio_format)
    return payload, _stream_status(payload)


def _resolve_audio(video_id: str) -> PlaybackResolveResponse:
    last_error: Exception | None = None
    unavailable_error: subprocess.TimeoutExpired | FileNotFoundError | None = None

    for resolution_pass in range(1, _RESOLUTION_PASSES + 1):
        retryable_failure = False

        logger.info(
            "resolving audio format candidates pass=%s candidates=%s",
            resolution_pass,
            len(_AUDIO_FORMATS),
        )
        executor = ThreadPoolExecutor(
            max_workers=len(_AUDIO_FORMATS),
            thread_name_prefix="yt-dlp",
        )
        futures = {
            executor.submit(_resolve_candidate, video_id, audio_format): audio_format
            for audio_format in _AUDIO_FORMATS
        }

        try:
            for future in as_completed(futures):
                audio_format = futures[future]
                try:
                    payload, status_code = future.result()
                except subprocess.CalledProcessError as error:
                    last_error = error
                    logger.warning(
                        "audio format could not be resolved pass=%s candidate=%s "
                        "error_type=%s",
                        resolution_pass,
                        audio_format,
                        type(error).__name__,
                    )
                    continue
                except (subprocess.TimeoutExpired, FileNotFoundError) as error:
                    unavailable_error = error
                    logger.warning(
                        "audio format provider unavailable pass=%s candidate=%s "
                        "error_type=%s",
                        resolution_pass,
                        audio_format,
                        type(error).__name__,
                    )
                    continue
                except requests.RequestException as error:
                    last_error = error
                    retryable_failure = True
                    logger.warning(
                        "audio format validation failed pass=%s candidate=%s "
                        "error_type=%s",
                        resolution_pass,
                        audio_format,
                        type(error).__name__,
                    )
                    continue

                media_host, client, cdn, expires, url_id = _media_url_log_fields(
                    payload
                )
                if status_code in (200, 206):
                    logger.info(
                        "selected audio format pass=%s candidate=%s format_id=%s "
                        "container=%s codec=%s bitrate_kbps=%s media_host=%s "
                        "client=%s cdn=%s expires=%s url_id=%s",
                        resolution_pass,
                        audio_format,
                        payload.format_id,
                        payload.ext,
                        payload.acodec,
                        payload.abr,
                        media_host,
                        client,
                        cdn,
                        expires,
                        url_id,
                    )
                    return _playback_response(payload)

                retryable_failure |= _is_retryable_status(status_code)
                logger.warning(
                    "resolved media URL was rejected pass=%s candidate=%s "
                    "format_id=%s container=%s codec=%s bitrate_kbps=%s "
                    "status=%s media_host=%s client=%s cdn=%s expires=%s "
                    "url_id=%s",
                    resolution_pass,
                    audio_format,
                    payload.format_id,
                    payload.ext,
                    payload.acodec,
                    payload.abr,
                    status_code,
                    media_host,
                    client,
                    cdn,
                    expires,
                    url_id,
                )
        finally:
            executor.shutdown(wait=False, cancel_futures=True)

        if resolution_pass < _RESOLUTION_PASSES and retryable_failure:
            logger.warning(
                "all media URLs were rejected; retrying resolution next_pass=%s "
                "delay_ms=%s",
                resolution_pass + 1,
                round(_RETRY_DELAY_SECONDS * 1000),
            )
            time.sleep(_RETRY_DELAY_SECONDS)
            continue

        break

    if unavailable_error is not None:
        raise unavailable_error

    raise PlaybackResolutionError("Playback could not be resolved") from last_error


class YouTubeMusicPlayback:
    def resolve(
        self,
        source: SourceIdentity,
    ) -> PlaybackResolveResponse:
        if source.provider_id != "youtube_music":
            raise UnsupportedProviderError(source.provider_id)

        try:
            return _resolve_audio(source.external_id)
        except (subprocess.TimeoutExpired, FileNotFoundError) as error:
            raise ProviderUnavailableError(
                "The provider is temporarily unavailable"
            ) from error
