import os
from typing import Any

import requests

from app.generated.resolver_v1 import ErrorCode
from app.hosts import HostLimiter
from app.metadata.document import DocumentError

AUDIODB_KEY_VARIABLE = "WHIO_RESOLVER_AUDIODB_KEY"
AUDIODB_HOST = "https://www.theaudiodb.com"
AUDIODB_PACE_HOST = "www.theaudiodb.com"
AUDIODB_ARTIST_MBID_PATH = "/api/v1/json/{key}/artist-mb.php"
AUDIODB_TIMEOUT_SECONDS = 20.0
AUDIODB_MINIMUM_INTERVAL_SECONDS = 3.0


def configured_key() -> str | None:
    value = os.environ.get(AUDIODB_KEY_VARIABLE)
    if value is None or not value.strip():
        return None
    return value.strip()


class AudioDbClient:
    def __init__(
        self,
        key: str,
        session: requests.Session | None = None,
        timeout: float = AUDIODB_TIMEOUT_SECONDS,
        minimum_interval: float = AUDIODB_MINIMUM_INTERVAL_SECONDS,
        limiter: HostLimiter | None = None,
    ) -> None:
        self._key = key
        self._session = requests.Session() if session is None else session
        self._timeout = timeout
        self._minimum_interval = minimum_interval
        self._limiter = HostLimiter() if limiter is None else limiter

    def fetch_artist_by_mbid(self, mbid: str) -> Any:
        url = AUDIODB_HOST + AUDIODB_ARTIST_MBID_PATH.format(key=self._key)

        with self._limiter.serialize(
            AUDIODB_PACE_HOST, self._minimum_interval
        ):
            try:
                response = self._session.get(
                    url, params={"i": mbid}, timeout=self._timeout
                )
            except requests.RequestException:
                raise DocumentError(
                    ErrorCode.provider_unavailable, True
                ) from None

            if response.status_code == 429:
                raise DocumentError(
                    ErrorCode.provider_unavailable,
                    True,
                    retry_after_seconds=parse_retry_after(
                        response.headers.get("Retry-After")
                    ),
                )
            if response.status_code >= 500:
                raise DocumentError(ErrorCode.provider_unavailable, True)
            if response.status_code >= 400:
                raise DocumentError(ErrorCode.resolution_failed, False)

            try:
                return response.json()
            except ValueError:
                raise DocumentError(
                    ErrorCode.resolution_failed, False
                ) from None


def parse_retry_after(value: Any) -> float | None:
    if not isinstance(value, str):
        return None
    try:
        seconds = float(value.strip())
    except ValueError:
        return None
    if seconds < 0:
        return None
    return seconds
