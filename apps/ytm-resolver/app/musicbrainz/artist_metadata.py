import re
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from html import unescape
from typing import Any, TypeGuard

from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    ImageDescriptor,
    LicenceDescriptor,
    MatchBasis,
    SourceIdentity,
)
from app.hosts import HostLimiter
from app.metadata.document import (
    BIOGRAPHY_CONTENT_LIMIT_BYTES,
    BIOGRAPHY_SUMMARY_LIMIT_BYTES,
    FIELD_TOO_LARGE_MESSAGE,
    ArtistDocument,
    Candidate,
    DocumentError,
    FieldCandidate,
    FieldError,
    exceeds_cap,
    select_requested_fields,
)
from app.musicbrainz.client import (
    MUSICBRAINZ_PROVIDER,
    MusicBrainzClient,
    commons_file_name,
    commons_file_name_from_page,
    commons_file_page,
    musicbrainz_artist_page,
    wikipedia_host_language,
    wikipedia_page_url,
    wikidata_entity_id_from_page,
)
from app.musicbrainz.wikipedia import (
    DISAMBIGUATION_PAGE_TYPE,
    WikipediaSummary,
    parse_extract,
    parse_summary,
    sitelink_wiki_target,
    wikipedia_target,
)

COMMONS_PROVIDER = "commons"
WIKIPEDIA_PROVIDER = "wikipedia"

IDENTITY_MBID_FIELD = "identity.mbid"
PAGE_URL_FIELD = "page.url"
TAG_FIELD = "tag"
BIOGRAPHY_SUMMARY_FIELD = "biography.summary"
BIOGRAPHY_CONTENT_FIELD = "biography.content"
BIOGRAPHY_LANGUAGE_FIELD = "biography.language"
PORTRAIT_FIELD = "image.portrait"

BIOGRAPHY_FIELDS = (
    BIOGRAPHY_SUMMARY_FIELD,
    BIOGRAPHY_CONTENT_FIELD,
    BIOGRAPHY_LANGUAGE_FIELD,
)

CHAIN_FIELDS = frozenset(
    {
        BIOGRAPHY_SUMMARY_FIELD,
        BIOGRAPHY_CONTENT_FIELD,
        BIOGRAPHY_LANGUAGE_FIELD,
        PORTRAIT_FIELD,
    }
)

PAGE_URL_RELATION_TYPES = frozenset(
    {
        "official homepage",
        "wikipedia",
        "wikidata",
        "discogs",
        "last.fm",
    }
)
WIKIDATA_RELATION_TYPE = "wikidata"
WIKIPEDIA_RELATION_TYPE = "wikipedia"
IMAGE_RELATION_TYPE = "image"

P18_PROPERTY = "P18"

TAG_MEMBERS = ("tags", "genres")
TAG_LIMIT_ENTRIES = 32
TAG_LIMIT_BYTES = 128

PORTRAIT_ROLE = "portrait"

MUSICBRAINZ_CORE_LICENCE_ID = "musicbrainz.core"
MUSICBRAINZ_SUPPLEMENTARY_LICENCE_ID = "musicbrainz.supplementary"
WIKIPEDIA_LICENCE_ID = "wikipedia.cc-by-sa"
COMMONS_LICENCE_ID_PREFIX = "commons:"
COMMONS_LICENCE_ID_MAX_LENGTH = 200
COMMONS_TERMS_MEMBERS = (
    "LicenseShortName",
    "LicenseUrl",
    "Artist",
    "Credit",
    "AttributionRequired",
)
COMMONS_ATTRIBUTION_MEMBERS = ("Artist", "Credit")
COMMONS_ATTRIBUTION_MAX_LENGTH = 1000

MUSICBRAINZ_ATTRIBUTION_TEXT = "MusicBrainz"
MUSICBRAINZ_ATTRIBUTION_URL = "https://musicbrainz.org"

MARKUP_PATTERN = re.compile(r"<[^>]*>")
WHITESPACE_PATTERN = re.compile(r"\s+")
LICENCE_TOKEN_PATTERN = re.compile(r"[^a-z0-9]+")


@dataclass(frozen=True)
class CommonsFileTerms:
    file_name: str
    file_page: str
    file_url: str | None
    licence_short_name: str
    licence_url: str | None
    attribution_text: str | None
    attribution_required: bool
    share_alike: bool
    terms_basis: str
    uncertain: bool


class MusicBrainzArtistMetadata:
    _client: MusicBrainzClient

    def __init__(self, client: MusicBrainzClient) -> None:
        self._client = client

    def fetch_artist_document(
        self,
        source: SourceIdentity,
        language: str | None,
        fields: list[str] | None,
    ) -> ArtistDocument:
        retrieved_at = datetime.now(UTC)

        document = parse_artist(
            self._client.fetch_artist(source.external_id), source.external_id
        )
        relations = parse_relations(document)

        candidates: list[FieldCandidate] = [
            identity_candidate(source, producing_source(source)),
            *page_url_candidates(source, relations),
        ]
        errors: list[FieldError] = []

        tags = tag_candidate(source, document)
        if tags is not None:
            candidates.append(tags)

        if needs_chain(fields):
            chain_candidates, chain_errors = self._chain(
                source, relations, language
            )
            candidates.extend(chain_candidates)
            errors.extend(chain_errors)

        return ArtistDocument(
            address=source,
            retrieved_at=retrieved_at,
            candidates=select_requested_fields(candidates, fields),
            errors=select_requested_fields(errors, fields),
        )

    def _chain(
        self,
        source: SourceIdentity,
        relations: list[Mapping[str, Any]],
        language: str | None,
    ) -> tuple[list[FieldCandidate], list[FieldError]]:
        candidates: list[FieldCandidate] = []
        errors: list[FieldError] = []

        entity: Mapping[str, Any] | None = None
        p18_names: list[str] = []
        wikipedia_relation = wikipedia_relation_target(relations)
        entity_id = wikidata_entity_id(relations)
        if entity_id is not None:
            try:
                entity = parse_wikidata_entity(
                    self._client.fetch_wikidata_entity(entity_id), entity_id
                )
                p18_names = p18_file_names(entity)
            except DocumentError as error:
                entity = None
                errors.append(field_error(PORTRAIT_FIELD, error))
                if wikipedia_relation is None:
                    errors.extend(field_errors(BIOGRAPHY_FIELDS, error))

        file_names = distinct([*commons_image_names(relations), *p18_names])

        for rank, file_name in enumerate(file_names):
            try:
                terms = parse_commons_imageinfo(
                    self._client.fetch_commons_imageinfo(file_name), file_name
                )
            except DocumentError as error:
                errors.append(field_error(PORTRAIT_FIELD, error))
                continue
            if terms is None:
                errors.append(
                    FieldError(
                        field=PORTRAIT_FIELD,
                        code=ErrorCode.resolution_failed,
                        retryable=False,
                    )
                )
                continue
            candidates.append(portrait_candidate(terms, rank))

        target = wikipedia_relation
        if target is None and entity is not None:
            target = sitelink_wiki_target(entity.get("sitelinks"), language)
        if target is not None:
            biography_candidates, biography_errors = self._biography(target)
            candidates.extend(biography_candidates)
            errors.extend(biography_errors)

        return candidates, errors

    def _biography(
        self, target: tuple[str, str]
    ) -> tuple[list[FieldCandidate], list[FieldError]]:
        host, requested_title = target
        candidates: list[FieldCandidate] = []
        errors: list[FieldError] = []

        summary: WikipediaSummary | None = None
        try:
            summary = parse_summary(
                self._client.fetch_wikipedia_summary(host, requested_title)
            )
        except DocumentError as error:
            errors.append(field_error(BIOGRAPHY_SUMMARY_FIELD, error))

        if summary is not None and summary.page_type == DISAMBIGUATION_PAGE_TYPE:
            return [], []

        extract: str | None = None
        try:
            extract = parse_extract(
                self._client.fetch_wikipedia_extract(host, requested_title)
            )
        except DocumentError as error:
            errors.append(field_error(BIOGRAPHY_CONTENT_FIELD, error))

        title = requested_title
        page_url = wikipedia_page_url(host, requested_title)
        if summary is not None:
            if summary.title is not None and summary.title.strip():
                title = summary.title.strip()
            if summary.page_url is not None:
                page_url = summary.page_url

        emitted = False
        summary_text = summary.extract if summary is not None else None
        if is_prose(summary_text):
            if exceeds_cap(summary_text, BIOGRAPHY_SUMMARY_LIMIT_BYTES):
                errors.append(field_too_large(BIOGRAPHY_SUMMARY_FIELD))
            else:
                candidates.append(
                    biography_candidate(
                        BIOGRAPHY_SUMMARY_FIELD, title, page_url, summary_text
                    )
                )
                emitted = True

        if is_prose(extract):
            if exceeds_cap(extract, BIOGRAPHY_CONTENT_LIMIT_BYTES):
                errors.append(field_too_large(BIOGRAPHY_CONTENT_FIELD))
            else:
                candidates.append(
                    biography_candidate(
                        BIOGRAPHY_CONTENT_FIELD, title, page_url, extract
                    )
                )
                emitted = True

        if emitted:
            language = wikipedia_host_language(host)
            if language is not None:
                candidates.append(
                    biography_language_candidate(language, title, page_url)
                )

        return candidates, errors


def musicbrainz_artist_metadata(
    user_agent: str,
    wikimedia_user_agent: str | None = None,
    *,
    limiter: HostLimiter,
) -> MusicBrainzArtistMetadata:
    return MusicBrainzArtistMetadata(
        MusicBrainzClient(user_agent, wikimedia_user_agent, limiter=limiter)
    )


def needs_chain(fields: list[str] | None) -> bool:
    if fields is None:
        return True
    return any(field in CHAIN_FIELDS for field in fields)


def parse_artist(payload: Any, mbid: str) -> Mapping[str, Any]:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    document_id = payload.get("id")
    if not isinstance(document_id, str) or not document_id.strip():
        raise DocumentError(ErrorCode.resolution_failed, False)
    if document_id.strip() != mbid:
        raise DocumentError(ErrorCode.resolution_failed, False)

    return payload


def parse_relations(document: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    relations = document.get("relations")
    if relations is None:
        return []
    if not isinstance(relations, list):
        raise DocumentError(ErrorCode.resolution_failed, False)

    for relation in relations:
        if not isinstance(relation, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)
    return relations


def relation_target(relation: Mapping[str, Any]) -> tuple[str | None, str | None]:
    relation_type = relation.get("type")
    url = relation.get("url")
    resource = url.get("resource") if isinstance(url, Mapping) else None

    return (
        relation_type if isinstance(relation_type, str) else None,
        resource.strip()
        if isinstance(resource, str) and resource.strip()
        else None,
    )


def producing_source(source: SourceIdentity) -> CandidateSource:
    return CandidateSource(
        provider_id=MUSICBRAINZ_PROVIDER,
        external_id=source.external_id,
        url=musicbrainz_artist_page(source.external_id),
        match_basis=MatchBasis.identifier,
    )


def linked_source(source: SourceIdentity, url: str) -> CandidateSource:
    return CandidateSource(
        provider_id=MUSICBRAINZ_PROVIDER,
        external_id=source.external_id,
        url=url,
        match_basis=MatchBasis.established_source,
    )


def identity_candidate(
    source: SourceIdentity, producing: CandidateSource
) -> FieldCandidate:
    return FieldCandidate(
        field=IDENTITY_MBID_FIELD,
        source=producing,
        licence=musicbrainz_core_licence(),
        text=source.external_id,
    )


def page_url_candidates(
    source: SourceIdentity, relations: list[Mapping[str, Any]]
) -> list[FieldCandidate]:
    resources: list[str] = []
    for relation in relations:
        relation_type, resource = relation_target(relation)
        if resource is None or relation_type not in PAGE_URL_RELATION_TYPES:
            continue
        if resource in resources:
            continue
        resources.append(resource)

    return [
        FieldCandidate(
            field=PAGE_URL_FIELD,
            source=linked_source(source, resource),
            licence=musicbrainz_core_licence(),
            rank=rank,
        )
        for rank, resource in enumerate(resources)
    ]


def tag_candidate(
    source: SourceIdentity, document: Mapping[str, Any]
) -> FieldCandidate | None:
    names = tag_names(document)
    if not names:
        return None

    producing = producing_source(source)
    return FieldCandidate(
        field=TAG_FIELD,
        source=producing,
        licence=musicbrainz_supplementary_licence(),
        candidates=[
            Candidate(source=producing, name=name, rank=rank)
            for rank, name in enumerate(names)
        ],
        attribution=musicbrainz_attribution(),
    )


def tag_names(document: Mapping[str, Any]) -> list[str]:
    names: list[str] = []
    for member in TAG_MEMBERS:
        entries = document.get(member)
        if entries is None:
            continue
        if not isinstance(entries, list):
            raise DocumentError(ErrorCode.resolution_failed, False)
        for entry in entries:
            if not isinstance(entry, Mapping):
                raise DocumentError(ErrorCode.resolution_failed, False)
            name = entry.get("name")
            if not isinstance(name, str):
                raise DocumentError(ErrorCode.resolution_failed, False)
            name = name.strip()
            if not name or exceeds_cap(name, TAG_LIMIT_BYTES):
                continue
            names.append(name)
    return names[:TAG_LIMIT_ENTRIES]


def wikidata_entity_id(relations: list[Mapping[str, Any]]) -> str | None:
    for relation in relations:
        relation_type, resource = relation_target(relation)
        if relation_type != WIKIDATA_RELATION_TYPE or resource is None:
            continue
        entity_id = wikidata_entity_id_from_page(resource)
        if entity_id is not None:
            return entity_id
    return None


def wikipedia_relation_target(
    relations: list[Mapping[str, Any]],
) -> tuple[str, str] | None:
    for relation in relations:
        relation_type, resource = relation_target(relation)
        if relation_type != WIKIPEDIA_RELATION_TYPE or resource is None:
            continue
        target = wikipedia_target(resource)
        if target is not None:
            return target
    return None


def commons_image_names(relations: list[Mapping[str, Any]]) -> list[str]:
    names: list[str] = []
    for relation in relations:
        relation_type, resource = relation_target(relation)
        if relation_type != IMAGE_RELATION_TYPE or resource is None:
            continue
        file_name = commons_file_name_from_page(resource)
        if file_name is not None:
            names.append(file_name)
    return names


def parse_wikidata_entity(payload: Any, entity_id: str) -> Mapping[str, Any]:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    entities = payload.get("entities")
    if not isinstance(entities, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    entity = entities.get(entity_id)
    if entity is None:
        return {}
    if not isinstance(entity, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)
    return entity


def p18_file_names(entity: Mapping[str, Any]) -> list[str]:
    claims = entity.get("claims")
    if claims is None:
        return []
    if not isinstance(claims, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    values = claims.get(P18_PROPERTY)
    if values is None:
        return []
    if not isinstance(values, list):
        raise DocumentError(ErrorCode.resolution_failed, False)

    names: list[str] = []
    for claim in values:
        if not isinstance(claim, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)
        file_name = claim_file_name(claim)
        if file_name is not None:
            names.append(file_name)
    return names


def claim_file_name(claim: Mapping[str, Any]) -> str | None:
    mainsnak = claim.get("mainsnak")
    if not isinstance(mainsnak, Mapping):
        return None
    datavalue = mainsnak.get("datavalue")
    if not isinstance(datavalue, Mapping):
        return None
    return commons_file_name(datavalue.get("value"))


def parse_commons_imageinfo(payload: Any, file_name: str) -> CommonsFileTerms | None:
    if not isinstance(payload, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)

    entries = imageinfo_entries(payload, file_name)
    if entries is None:
        return None

    entry = entries[0]
    extmetadata = entry.get("extmetadata")
    if not isinstance(extmetadata, Mapping):
        return None

    short_name = extmetadata_text(extmetadata, "LicenseShortName")
    if short_name is None:
        return None
    if len(COMMONS_LICENCE_ID_PREFIX + short_name) > COMMONS_LICENCE_ID_MAX_LENGTH:
        return None

    members = tuple(
        member for member in COMMONS_TERMS_MEMBERS if member in extmetadata
    )
    attribution_required = attribution_required_value(extmetadata)
    attribution_text = commons_attribution_text(extmetadata)
    file_page = description_page(entry, file_name)

    return CommonsFileTerms(
        file_name=file_name,
        file_page=file_page,
        file_url=url_member(entry, "url"),
        licence_short_name=short_name,
        licence_url=extmetadata_text(extmetadata, "LicenseUrl"),
        attribution_text=attribution_text,
        attribution_required=attribution_required,
        share_alike=licence_share_alike(short_name),
        terms_basis="Commons imageinfo extmetadata: " + ", ".join(members),
        uncertain=(
            "AttributionRequired" not in extmetadata
            or (attribution_required and attribution_text is None)
        ),
    )


def imageinfo_entries(
    payload: Mapping[str, Any], file_name: str
) -> list[Mapping[str, Any]] | None:
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
        title = page.get("title")
        if isinstance(title, str) and commons_file_name(title) != file_name:
            continue
        entries = page.get("imageinfo")
        if entries is None:
            continue
        if not isinstance(entries, list):
            raise DocumentError(ErrorCode.resolution_failed, False)
        if not entries:
            continue
        if not isinstance(entries[0], Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)
        return entries
    return None


def extmetadata_text(extmetadata: Mapping[str, Any], member: str) -> str | None:
    value = extmetadata_value(extmetadata.get(member))
    if not isinstance(value, str):
        return None
    text = value.strip()
    return text or None


def extmetadata_value(entry: Any) -> Any:
    if isinstance(entry, Mapping) and "value" in entry:
        return entry["value"]
    return entry


def attribution_required_value(extmetadata: Mapping[str, Any]) -> bool:
    value = extmetadata_value(extmetadata.get("AttributionRequired"))
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"true", "yes", "1"}
    return True


def commons_attribution_text(extmetadata: Mapping[str, Any]) -> str | None:
    for member in COMMONS_ATTRIBUTION_MEMBERS:
        text = strip_markup(extmetadata_text(extmetadata, member))
        if text is not None and len(text) <= COMMONS_ATTRIBUTION_MAX_LENGTH:
            return text
    return None


def strip_markup(value: str | None) -> str | None:
    if value is None:
        return None
    text = unescape(MARKUP_PATTERN.sub(" ", value))
    return WHITESPACE_PATTERN.sub(" ", text).strip() or None


def licence_share_alike(short_name: str) -> bool:
    tokens = {
        token
        for token in LICENCE_TOKEN_PATTERN.split(short_name.lower())
        if token
    }
    return "sa" in tokens


def url_member(entry: Mapping[str, Any], member: str) -> str | None:
    value = entry.get(member)
    if isinstance(value, str) and value.strip().startswith(("http://", "https://")):
        return value.strip()
    return None


def description_page(entry: Mapping[str, Any], file_name: str) -> str:
    page = url_member(entry, "descriptionurl")
    if page is not None:
        return page
    return commons_file_page(file_name)


def portrait_candidate(terms: CommonsFileTerms, rank: int) -> FieldCandidate:
    return FieldCandidate(
        field=PORTRAIT_FIELD,
        source=CandidateSource(
            provider_id=COMMONS_PROVIDER,
            external_id=terms.file_name,
            url=terms.file_page,
            match_basis=MatchBasis.established_source,
        ),
        licence=commons_licence(terms),
        image=ImageDescriptor(
            role=PORTRAIT_ROLE,
            reference=terms.file_url if terms.file_url is not None else terms.file_page,
        ),
        rank=rank,
        attribution=(
            Attribution(text=terms.attribution_text)
            if terms.attribution_text is not None
            else None
        ),
    )


def biography_candidate(
    field: str, title: str, page_url: str, text: str
) -> FieldCandidate:
    return FieldCandidate(
        field=field,
        source=wikipedia_source(title, page_url),
        licence=wikipedia_licence(),
        text=text,
        attribution=wikipedia_attribution(title, page_url),
    )


def biography_language_candidate(
    language: str, title: str, page_url: str
) -> FieldCandidate:
    return FieldCandidate(
        field=BIOGRAPHY_LANGUAGE_FIELD,
        source=wikipedia_source(title, page_url),
        licence=wikipedia_licence(),
        language=language,
        attribution=wikipedia_attribution(title, page_url),
    )


def wikipedia_source(title: str, page_url: str) -> CandidateSource:
    return CandidateSource(
        provider_id=WIKIPEDIA_PROVIDER,
        external_id=title,
        url=page_url,
        match_basis=MatchBasis.established_source,
    )


def wikipedia_attribution(title: str, page_url: str) -> Attribution:
    return Attribution(text=title, url=page_url)


def is_prose(value: str | None) -> TypeGuard[str]:
    return value is not None and bool(value.strip())


def distinct(values: list[str]) -> list[str]:
    seen: set[str] = set()
    result: list[str] = []
    for value in values:
        if value in seen:
            continue
        seen.add(value)
        result.append(value)
    return result


def field_error(field: str, error: DocumentError) -> FieldError:
    return FieldError(field=field, code=error.code, retryable=error.retryable)


def field_errors(fields: tuple[str, ...], error: DocumentError) -> list[FieldError]:
    return [field_error(field, error) for field in fields]


def field_too_large(field: str) -> FieldError:
    return FieldError(
        field=field,
        code=ErrorCode.resolution_failed,
        retryable=False,
        message=FIELD_TOO_LARGE_MESSAGE,
    )


def musicbrainz_attribution() -> Attribution:
    return Attribution(
        text=MUSICBRAINZ_ATTRIBUTION_TEXT, url=MUSICBRAINZ_ATTRIBUTION_URL
    )


def musicbrainz_core_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=MUSICBRAINZ_CORE_LICENCE_ID,
        name="CC0 1.0 Universal (Public Domain Dedication)",
        url="https://creativecommons.org/publicdomain/zero/1.0/",
        attribution_required=False,
        redistribution_allowed=True,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        terms_basis="MusicBrainz core data is dedicated to the public domain under CC0",
        uncertainty=False,
    )


def musicbrainz_supplementary_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=MUSICBRAINZ_SUPPLEMENTARY_LICENCE_ID,
        name="CC BY-NC-SA 3.0",
        url="https://creativecommons.org/licenses/by-nc-sa/3.0/",
        attribution_required=True,
        redistribution_allowed=True,
        cache_allowed=True,
        share_alike=True,
        non_commercial=True,
        terms_basis=(
            "MusicBrainz supplementary data (tags and genres) is licensed "
            "CC BY-NC-SA 3.0"
        ),
        uncertainty=False,
    )


def wikipedia_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=WIKIPEDIA_LICENCE_ID,
        name="CC BY-SA 4.0",
        url="https://creativecommons.org/licenses/by-sa/4.0/",
        attribution_required=True,
        redistribution_allowed=True,
        cache_allowed=True,
        share_alike=True,
        non_commercial=False,
        terms_basis=(
            "Wikipedia text is licensed CC BY-SA 4.0 and the GFDL; "
            "attribution and a link back are required"
        ),
        uncertainty=False,
    )


def commons_licence(terms: CommonsFileTerms) -> LicenceDescriptor:
    return LicenceDescriptor(
        id=COMMONS_LICENCE_ID_PREFIX + terms.licence_short_name,
        name=terms.licence_short_name,
        url=terms.licence_url if terms.licence_url is not None else terms.file_page,
        attribution_required=terms.attribution_required,
        redistribution_allowed=True,
        cache_allowed=True,
        share_alike=terms.share_alike,
        non_commercial=False,
        terms_basis=terms.terms_basis,
        uncertainty=terms.uncertain,
    )
