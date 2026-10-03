import json
import os
from collections.abc import Mapping
from typing import Any
from urllib.parse import quote

import requests

from app.generated.resolver_v1 import ErrorCode
from app.hosts import HostLimiter
from app.metadata.document import DocumentError

LISTENBRAINZ_PROVIDER = "listenbrainz"

LISTENBRAINZ_TOKEN_VARIABLE = "WHIO_RESOLVER_LISTENBRAINZ_TOKEN"
LISTENBRAINZ_ALGORITHM_VARIABLE = "WHIO_METADATA_ALGORITHM"

LISTENBRAINZ_DEFAULT_ALGORITHM = (
    "session_based_days_7500_session_300_contribution_5_threshold_10"
    "_limit_100_filter_True_skip_30"
)

LISTENBRAINZ_LABS_HOST = "labs.api.listenbrainz.org"
LISTENBRAINZ_LABS_BASE_URL = "https://labs.api.listenbrainz.org"
LISTENBRAINZ_SIMILAR_ARTISTS_PATH = "/similar-artists/json"

LISTENBRAINZ_API_HOST = "api.listenbrainz.org"
LISTENBRAINZ_API_BASE_URL = "https://api.listenbrainz.org"
LISTENBRAINZ_TOP_RECORDINGS_PATH = "/1/popularity/top-recordings-for-artist/{mbid}"
LISTENBRAINZ_ARTIST_POPULARITY_PATH = "/1/popularity/artist"

LISTENBRAINZ_TIMEOUT_SECONDS = 20.0
LISTENBRAINZ_MINIMUM_INTERVAL_SECONDS = 1.0
LISTENBRAINZ_PACE_HOST = LISTENBRAINZ_API_HOST

INSTANCE_DISCOVERY_ENTRY_LIMIT = 50
TOP_RECORDINGS_CHUNK_SIZE = 65536


def configured_token() -> str | None:
    return configured_value(LISTENBRAINZ_TOKEN_VARIABLE)


def configured_algorithm() -> str:
    value = configured_value(LISTENBRAINZ_ALGORITHM_VARIABLE)
    if value is None:
        return LISTENBRAINZ_DEFAULT_ALGORITHM
    return value


def configured_value(variable: str) -> str | None:
    value = os.environ.get(variable)
    if value is None or not value.strip():
        return None
    return value.strip()


class ListenBrainzClient:
    def __init__(
        self,
        token: str | None,
        user_agent: str,
        algorithm: str = LISTENBRAINZ_DEFAULT_ALGORITHM,
        session: requests.Session | None = None,
        timeout: float = LISTENBRAINZ_TIMEOUT_SECONDS,
        minimum_interval: float = LISTENBRAINZ_MINIMUM_INTERVAL_SECONDS,
        limiter: HostLimiter | None = None,
    ) -> None:
        self._token = token
        self._user_agent = user_agent
        self._algorithm = algorithm
        self._session = requests.Session() if session is None else session
        self._timeout = timeout
        self._minimum_interval = minimum_interval
        self._limiter = HostLimiter() if limiter is None else limiter

    @property
    def algorithm(self) -> str:
        return self._algorithm

    def fetch_similar_artists(self, mbid: str) -> Any:
        response = self._send(
            LISTENBRAINZ_LABS_BASE_URL + LISTENBRAINZ_SIMILAR_ARTISTS_PATH,
            params={"artist_mbids": mbid, "algorithm": self._algorithm},
        )
        return parse_json(response)

    def fetch_top_recordings(self, mbid: str) -> list[Any]:
        response = self._send(
            LISTENBRAINZ_API_BASE_URL
            + LISTENBRAINZ_TOP_RECORDINGS_PATH.format(mbid=quote(mbid, safe="")),
            authorized=True,
            stream=True,
        )

        try:
            return stream_json_list(response, INSTANCE_DISCOVERY_ENTRY_LIMIT)
        except requests.RequestException:
            raise DocumentError(ErrorCode.provider_unavailable, True) from None
        finally:
            response.close()

    def fetch_artist_popularity(self, mbid: str) -> Any:
        response = self._send(
            LISTENBRAINZ_API_BASE_URL + LISTENBRAINZ_ARTIST_POPULARITY_PATH,
            json_body={"artist_mbids": [mbid]},
            authorized=True,
        )
        return parse_json(response)

    def _send(
        self,
        url: str,
        params: Mapping[str, str] | None = None,
        json_body: Mapping[str, Any] | None = None,
        authorized: bool = False,
        stream: bool = False,
    ) -> requests.Response:
        headers = {"User-Agent": self._user_agent}
        if authorized and self._token is not None:
            headers["Authorization"] = "Token " + self._token

        members: dict[str, Any] = {"headers": headers, "timeout": self._timeout}
        if stream:
            members["stream"] = True
        if params is not None:
            members["params"] = dict(params)

        with self._limiter.serialize(
            LISTENBRAINZ_PACE_HOST, self._minimum_interval
        ):
            try:
                if json_body is None:
                    response = self._session.get(url, **members)
                else:
                    response = self._session.post(
                        url, json=dict(json_body), **members
                    )
            except requests.RequestException:
                raise DocumentError(
                    ErrorCode.provider_unavailable, True
                ) from None

            try:
                raise_for_status(response)
            except DocumentError:
                response.close()
                raise

            return response


def raise_for_status(response: requests.Response) -> None:
    if response.status_code == 404:
        raise DocumentError(ErrorCode.source_not_found, False)
    if response.status_code == 429:
        raise DocumentError(
            ErrorCode.provider_unavailable,
            True,
            retry_after_seconds=parse_retry_after(
                response.headers.get("Retry-After")
            ),
        )
    if response.status_code >= 500:
        raise DocumentError(
            ErrorCode.provider_unavailable,
            True,
            retry_after_seconds=parse_retry_after(
                response.headers.get("Retry-After")
            ),
        )
    if response.status_code >= 400:
        raise DocumentError(ErrorCode.resolution_failed, False)


def parse_json(response: requests.Response) -> Any:
    try:
        return response.json()
    except ValueError:
        raise DocumentError(ErrorCode.resolution_failed, False) from None


def stream_json_list(response: requests.Response, limit: int) -> list[Any]:
    decoder = json.JSONDecoder()
    text = ""
    items: list[Any] = []
    index = 0
    opened = False

    for chunk in response.iter_content(
        chunk_size=TOP_RECORDINGS_CHUNK_SIZE, decode_unicode=True
    ):
        if not chunk:
            continue
        text += chunk
        while True:
            if not opened:
                offset = len(text) - len(text.lstrip())
                if offset >= len(text):
                    break
                if text[offset] != "[":
                    raise DocumentError(ErrorCode.resolution_failed, False)
                opened = True
                index = offset + 1

            length = len(text)
            while index < length and (text[index].isspace() or text[index] == ","):
                index += 1
            if index >= length:
                break
            if text[index] == "]":
                return items

            try:
                value, end = decoder.raw_decode(text, index)
            except ValueError:
                break

            items.append(value)
            index = end
            if len(items) >= limit:
                return items

    if opened:
        length = len(text)
        while index < length and text[index].isspace():
            index += 1
        if index < length and text[index] == "]":
            return items

    raise DocumentError(ErrorCode.resolution_failed, False)


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
