from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any
from urllib.parse import unquote, urlparse

from app.generated.resolver_v1 import ErrorCode
from app.metadata.document import DocumentError
from app.musicbrainz.client import (
    WIKIPEDIA_HOST_SUFFIX,
    host_without_port,
)

WIKIPEDIA_SITE_SUFFIX = "wiki"
WIKIPEDIA_PATH_PREFIX = "/wiki/"
DEFAULT_SITELINK_LANGUAGE = "en"
DISAMBIGUATION_PAGE_TYPE = "disambiguation"


@dataclass(frozen=True)
class WikipediaSummary:
    page_type: str | None
    extract: str | None
    title: str | None
    page_url: str | None


def wikipedia_host(language: str) -> str:
    return language + WIKIPEDIA_HOST_SUFFIX


def wikipedia_target(url: str) -> tuple[str, str] | None:
    parsed = urlparse(url)
    host = host_without_port(parsed.netloc)
    if not host.endswith(WIKIPEDIA_HOST_SUFFIX):
        return None
    if not parsed.path.startswith(WIKIPEDIA_PATH_PREFIX):
        return None
    title = (
        unquote(parsed.path[len(WIKIPEDIA_PATH_PREFIX) :])
        .replace("_", " ")
        .strip()
    )
    if not title:
        return None
    return host, title


def parse_summary(payload: Any) -> WikipediaSummary:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    return WikipediaSummary(
        page_type=optional_text(payload, "type"),
        extract=optional_text(payload, "extract"),
        title=optional_text(payload, "title"),
        page_url=summary_page_url(payload.get("content_urls")),
    )


def parse_extract(payload: Any) -> str | None:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    query = payload.get("query")
    if query is None:
        return None
    if not isinstance(query, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    pages = query.get("pages")
    if pages is None:
        return None
    if not isinstance(pages, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    for page in pages.values():
        if not isinstance(page, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)
        extract = page.get("extract")
        if extract is None:
            continue
        if not isinstance(extract, str):
            raise DocumentError(ErrorCode.resolution_failed, False)
        if extract.strip():
            return extract
    return None


def sitelink_wiki_target(
    sitelinks: Any, language: str | None
) -> tuple[str, str] | None:
    if not isinstance(sitelinks, Mapping):
        return None

    for candidate in sitelink_languages(language):
        entry = sitelinks.get(candidate + WIKIPEDIA_SITE_SUFFIX)
        if not isinstance(entry, Mapping):
            continue
        title = entry.get("title")
        if isinstance(title, str) and title.strip():
            return wikipedia_host(candidate), title.strip()
    return None


def sitelink_languages(language: str | None) -> tuple[str, ...]:
    if language is None or language == DEFAULT_SITELINK_LANGUAGE:
        return (DEFAULT_SITELINK_LANGUAGE,)
    return (language, DEFAULT_SITELINK_LANGUAGE)


def optional_text(payload: Mapping[str, Any], member: str) -> str | None:
    value = payload.get(member)
    if value is None:
        return None
    if not isinstance(value, str):
        raise DocumentError(ErrorCode.resolution_failed, False)
    return value


def summary_page_url(content_urls: Any) -> str | None:
    if not isinstance(content_urls, Mapping):
        return None
    desktop = content_urls.get("desktop")
    if not isinstance(desktop, Mapping):
        return None
    page = desktop.get("page")
    if isinstance(page, str) and page.strip().startswith(("http://", "https://")):
        return page.strip()
    return None
