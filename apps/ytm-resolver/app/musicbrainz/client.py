import os
import time
from collections.abc import Mapping
from typing import Any
from urllib.parse import quote, urlparse

import requests

from app.generated.resolver_v1 import ErrorCode
from app.hosts import HostLimiter
from app.metadata.document import DocumentError

MUSICBRAINZ_PROVIDER = "musicbrainz"

MUSICBRAINZ_USER_AGENT_VARIABLE = "WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT"
WIKIMEDIA_USER_AGENT_VARIABLE = "WHIO_RESOLVER_WIKIMEDIA_USER_AGENT"

MUSICBRAINZ_HOST = "musicbrainz.org"
MUSICBRAINZ_BASE_URL = "https://musicbrainz.org"
MUSICBRAINZ_ARTIST_PATH = "/ws/2/artist/{mbid}"
MUSICBRAINZ_ARTIST_INCLUDES = "url-rels+tags+genres"
MUSICBRAINZ_ARTIST_PAGE = "https://musicbrainz.org/artist/{mbid}"

WIKIDATA_HOST = "www.wikidata.org"
WIKIDATA_BASE_URL = "https://www.wikidata.org"
WIKIDATA_API_PATH = "/w/api.php"
WIKIDATA_ENTITY_PROPS = "claims|sitelinks"
WIKIDATA_ENTITY_PREFIX = "Q"

COMMONS_HOST = "commons.wikimedia.org"
COMMONS_BASE_URL = "https://commons.wikimedia.org"
COMMONS_API_PATH = "/w/api.php"
COMMONS_FILE_NAMESPACE = "File:"
COMMONS_FILE_PAGE = "https://commons.wikimedia.org/wiki/File:{file_name}"

WIKIPEDIA_HOST_SUFFIX = ".wikipedia.org"
WIKIPEDIA_BASE_URL = "https://{host}"
WIKIPEDIA_SUMMARY_PATH = "/api/rest_v1/page/summary/{title}"
WIKIPEDIA_API_PATH = "/w/api.php"
WIKIPEDIA_PAGE = "https://{host}/wiki/{title}"
WIKIPEDIA_HOST_LABEL_INDEX = 0
WIKIPEDIA_LANGUAGE_MINIMUM_LENGTH = 2

WIKIMEDIA_MAXLAG = "5"

MUSICBRAINZ_TIMEOUT_SECONDS = 20.0
WIKIMEDIA_TIMEOUT_SECONDS = 20.0
MUSICBRAINZ_MINIMUM_INTERVAL_SECONDS = 1.0
WIKIMEDIA_MINIMUM_INTERVAL_SECONDS = 0.0


def configured_user_agent() -> str | None:
    return configured_value(MUSICBRAINZ_USER_AGENT_VARIABLE)


def configured_wikimedia_user_agent() -> str | None:
    return configured_value(WIKIMEDIA_USER_AGENT_VARIABLE)


def configured_value(variable: str) -> str | None:
    value = os.environ.get(variable)
    if value is None or not value.strip():
        return None
    return value.strip()


def musicbrainz_artist_page(mbid: str) -> str:
    return MUSICBRAINZ_ARTIST_PAGE.format(mbid=quote(mbid, safe=""))


def commons_file_page(file_name: str) -> str:
    return COMMONS_FILE_PAGE.format(file_name=file_path_segment(file_name))


def commons_file_title(file_name: str) -> str:
    return COMMONS_FILE_NAMESPACE + file_name


def commons_file_name_from_page(url: str) -> str | None:
    parsed = urlparse(url)
    if host_without_port(parsed.netloc) != COMMONS_HOST:
        return None
    path = parsed.path
    if not path.startswith("/wiki/"):
        return None
    title = path[len("/wiki/") :]
    prefix = COMMONS_FILE_NAMESPACE.lower()
    if not title.lower().startswith(prefix):
        return None
    name = title[len(COMMONS_FILE_NAMESPACE) :].replace("_", " ").strip()
    if not name:
        return None
    return name


def commons_file_name(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    name = value.strip().replace("_", " ")
    prefix = COMMONS_FILE_NAMESPACE.lower()
    if name.lower().startswith(prefix):
        name = name[len(COMMONS_FILE_NAMESPACE) :].strip()
    if not name:
        return None
    return name


def wikidata_entity_id_from_page(url: str) -> str | None:
    parsed = urlparse(url)
    if host_without_port(parsed.netloc) not in {WIKIDATA_HOST, "wikidata.org"}:
        return None
    path = parsed.path
    if not path.startswith("/wiki/"):
        return None
    entity_id = path[len("/wiki/") :].strip()
    if not entity_id.startswith(WIKIDATA_ENTITY_PREFIX):
        return None
    number = entity_id[len(WIKIDATA_ENTITY_PREFIX) :]
    if not number.isdigit():
        return None
    return entity_id


def wikipedia_page_url(host: str, title: str) -> str:
    return WIKIPEDIA_PAGE.format(host=host, title=file_path_segment(title))


def wikipedia_host_language(host: str) -> str | None:
    if not host.endswith(WIKIPEDIA_HOST_SUFFIX):
        return None
    label = host[: -len(WIKIPEDIA_HOST_SUFFIX)]
    if not label:
        return None
    language = label.split(".")[WIKIPEDIA_HOST_LABEL_INDEX]
    if len(language) < WIKIPEDIA_LANGUAGE_MINIMUM_LENGTH:
        return None
    return language


def file_path_segment(value: str) -> str:
    return quote(value.replace(" ", "_"), safe="")


def host_without_port(netloc: str) -> str:
    host = netloc.lower()
    if "@" in host:
        host = host.rsplit("@", 1)[1]
    if ":" in host:
        host = host.split(":", 1)[0]
    return host


class MusicBrainzClient:
    def __init__(
        self,
        user_agent: str,
        wikimedia_user_agent: str | None = None,
        session: requests.Session | None = None,
        musicbrainz_timeout: float = MUSICBRAINZ_TIMEOUT_SECONDS,
        wikimedia_timeout: float = WIKIMEDIA_TIMEOUT_SECONDS,
        musicbrainz_minimum_interval: float = MUSICBRAINZ_MINIMUM_INTERVAL_SECONDS,
        wikimedia_minimum_interval: float = WIKIMEDIA_MINIMUM_INTERVAL_SECONDS,
        limiter: HostLimiter | None = None,
    ) -> None:
        self._user_agent = user_agent
        self._wikimedia_user_agent = wikimedia_user_agent or user_agent
        self._session = requests.Session() if session is None else session
        self._musicbrainz_timeout = musicbrainz_timeout
        self._wikimedia_timeout = wikimedia_timeout
        self._musicbrainz_minimum_interval = musicbrainz_minimum_interval
        self._wikimedia_minimum_interval = wikimedia_minimum_interval
        self._limiter = HostLimiter() if limiter is None else limiter

    def fetch_artist(self, mbid: str) -> Any:
        return self._get_json(
            MUSICBRAINZ_HOST,
            MUSICBRAINZ_BASE_URL + MUSICBRAINZ_ARTIST_PATH.format(
                mbid=quote(mbid, safe="")
            ),
            params={
                "inc": MUSICBRAINZ_ARTIST_INCLUDES,
                "fmt": "json",
            },
            timeout=self._musicbrainz_timeout,
            user_agent=self._user_agent,
            minimum_interval=self._musicbrainz_minimum_interval,
            missing_source_code=ErrorCode.source_not_found,
        )

    def fetch_wikidata_entity(self, entity_id: str) -> Any:
        return self._get_json(
            WIKIDATA_HOST,
            WIKIDATA_BASE_URL + WIKIDATA_API_PATH,
            params={
                "action": "wbgetentities",
                "ids": entity_id,
                "props": WIKIDATA_ENTITY_PROPS,
                "format": "json",
                "maxlag": WIKIMEDIA_MAXLAG,
            },
            timeout=self._wikimedia_timeout,
            user_agent=self._wikimedia_user_agent,
            minimum_interval=self._wikimedia_minimum_interval,
        )

    def fetch_commons_imageinfo(self, file_name: str) -> Any:
        return self._get_json(
            COMMONS_HOST,
            COMMONS_BASE_URL + COMMONS_API_PATH,
            params={
                "action": "query",
                "prop": "imageinfo",
                "iiprop": "extmetadata|url",
                "titles": commons_file_title(file_name),
                "format": "json",
                "maxlag": WIKIMEDIA_MAXLAG,
            },
            timeout=self._wikimedia_timeout,
            user_agent=self._wikimedia_user_agent,
            minimum_interval=self._wikimedia_minimum_interval,
        )

    def fetch_wikipedia_summary(self, host: str, title: str) -> Any:
        return self._get_json(
            host,
            WIKIPEDIA_BASE_URL.format(host=host)
            + WIKIPEDIA_SUMMARY_PATH.format(title=file_path_segment(title)),
            params={},
            timeout=self._wikimedia_timeout,
            user_agent=self._wikimedia_user_agent,
            minimum_interval=self._wikimedia_minimum_interval,
        )

    def fetch_wikipedia_extract(self, host: str, title: str) -> Any:
        return self._get_json(
            host,
            WIKIPEDIA_BASE_URL.format(host=host) + WIKIPEDIA_API_PATH,
            params={
                "action": "query",
                "prop": "extracts",
                "explaintext": "1",
                "titles": title,
                "format": "json",
                "maxlag": WIKIMEDIA_MAXLAG,
            },
            timeout=self._wikimedia_timeout,
            user_agent=self._wikimedia_user_agent,
            minimum_interval=self._wikimedia_minimum_interval,
        )

    def _get_json(
        self,
        host: str,
        url: str,
        params: Mapping[str, str],
        timeout: float,
        user_agent: str,
        minimum_interval: float,
        missing_source_code: ErrorCode | None = None,
    ) -> Any:
        with self._limiter.serialize(host, minimum_interval):
            try:
                response = self._session.get(
                    url,
                    params=dict(params),
                    timeout=timeout,
                    headers={"User-Agent": user_agent},
                )
            except requests.RequestException:
                raise DocumentError(
                    ErrorCode.provider_unavailable, True
                ) from None

            if response.status_code == 404 and missing_source_code is not None:
                raise DocumentError(missing_source_code, False)
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
