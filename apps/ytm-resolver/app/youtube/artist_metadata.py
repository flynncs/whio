from collections.abc import Mapping
from datetime import UTC, datetime
from typing import Any

from requests.exceptions import RequestException
from ytmusicapi import YTMusic
from ytmusicapi.exceptions import YTMusicServerError, YTMusicUserError

from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    ImageDescriptor,
    LicenceDescriptor,
    MatchBasis,
    SourceIdentity,
)
from app.youtube.artwork import full_size_artwork_url
from app.metadata.document import (
    BIOGRAPHY_CONTENT_LIMIT_BYTES,
    FIELD_TOO_LARGE_MESSAGE,
    ArtistDocument,
    Candidate,
    DocumentError,
    FieldCandidate,
    FieldError,
    exceeds_cap,
    select_requested_fields,
)

YOUTUBE_MUSIC_PROVIDER = "youtube_music"

BIOGRAPHY_CONTENT_FIELD = "biography.content"
WIDE_IMAGE_FIELD = "image.wide"
SIMILAR_FIELD = "similar"
TOP_TRACKS_FIELD = "top_tracks"
WIDE_IMAGE_ROLE = "wide"
DISCOVERY_CANDIDATE_LIMIT = 50

YOUTUBE_MUSIC_TERMS_ID = "youtube_music"
YOUTUBE_MUSIC_TERMS_NAME = "YouTube Terms of Service"
YOUTUBE_MUSIC_TERMS_URL = "https://www.youtube.com/t/terms"
YOUTUBE_MUSIC_TERMS_BASIS = (
    "YouTube Terms of Service grant no redistribution right for provider content"
)


class YouTubeMusicArtistMetadata:
    _client: YTMusic

    def __init__(self, client: YTMusic) -> None:
        self._client = client

    def fetch_artist_document(
        self,
        source: SourceIdentity,
        language: str | None,
        fields: list[str] | None,
    ) -> ArtistDocument:
        del language

        retrieved_at = datetime.now(UTC)

        try:
            document = self._client.get_artist(source.external_id)
        except (YTMusicUserError, KeyError) as error:
            raise DocumentError(ErrorCode.source_not_found, False) from error
        except (YTMusicServerError, RequestException) as error:
            raise DocumentError(ErrorCode.provider_unavailable, True) from error
        except (TypeError, AttributeError, IndexError, ValueError) as error:
            raise DocumentError(ErrorCode.resolution_failed, False) from error

        if not isinstance(document, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)

        candidates: list[FieldCandidate] = []
        errors: list[FieldError] = []

        for candidate, error in (
            parse_biography_candidate(source, document),
            parse_wide_image_candidate(source, document),
            parse_similar_candidate(source, document),
            parse_top_tracks_candidate(source, document),
        ):
            if candidate is not None:
                candidates.append(candidate)
            if error is not None:
                errors.append(error)

        return ArtistDocument(
            address=source,
            retrieved_at=retrieved_at,
            candidates=select_requested_fields(candidates, fields),
            errors=select_requested_fields(errors, fields),
        )


def parse_biography_candidate(
    source: SourceIdentity, document: Mapping[str, Any]
) -> tuple[FieldCandidate | None, FieldError | None]:
    description = document.get("description")
    if not isinstance(description, str) or not description.strip():
        return None, None
    if exceeds_cap(description, BIOGRAPHY_CONTENT_LIMIT_BYTES):
        return None, FieldError(
            field=BIOGRAPHY_CONTENT_FIELD,
            code=ErrorCode.resolution_failed,
            retryable=False,
            message=FIELD_TOO_LARGE_MESSAGE,
        )

    return (
        FieldCandidate(
            field=BIOGRAPHY_CONTENT_FIELD,
            source=producing_source(source),
            licence=parse_licence(),
            text=description,
            attribution=parse_attribution(document),
        ),
        None,
    )


def parse_wide_image_candidate(
    source: SourceIdentity, document: Mapping[str, Any]
) -> tuple[FieldCandidate | None, FieldError | None]:
    image = parse_largest_thumbnail(document.get("thumbnails"))
    if image is None:
        return None, None

    return (
        FieldCandidate(
            field=WIDE_IMAGE_FIELD,
            source=producing_source(source),
            licence=parse_licence(),
            image=image,
            attribution=parse_attribution(document),
        ),
        None,
    )


def parse_similar_candidate(
    source: SourceIdentity, document: Mapping[str, Any]
) -> tuple[FieldCandidate | None, FieldError | None]:
    entries = parse_section_entries(document.get("related"))
    if not entries:
        return None, None

    candidates = []
    for rank, entry in enumerate(entries):
        name = require_text(entry, "title")
        browse_id = require_text(entry, "browseId")
        candidates.append(
            Candidate(
                source=CandidateSource(
                    provider_id=YOUTUBE_MUSIC_PROVIDER,
                    external_id=browse_id,
                    match_basis=MatchBasis.identifier,
                ),
                name=name,
                rank=rank,
            )
        )

    return (
        FieldCandidate(
            field=SIMILAR_FIELD,
            source=producing_source(source),
            licence=parse_licence(),
            candidates=candidates,
            attribution=parse_attribution(document),
        ),
        None,
    )


def parse_top_tracks_candidate(
    source: SourceIdentity, document: Mapping[str, Any]
) -> tuple[FieldCandidate | None, FieldError | None]:
    entries = parse_section_entries(document.get("songs"))
    if not entries:
        return None, None

    candidates = []
    for rank, entry in enumerate(entries):
        name = require_text(entry, "title")
        video_id = require_text(entry, "videoId")
        candidates.append(
            Candidate(
                source=CandidateSource(
                    provider_id=YOUTUBE_MUSIC_PROVIDER,
                    external_id=video_id,
                    match_basis=MatchBasis.identifier,
                ),
                name=name,
                artists=parse_artist_names(entry.get("artists")),
                release_name=parse_release_name(entry.get("album")),
                duration_ms=parse_duration_ms(entry.get("duration_seconds")),
                isrc=parse_optional_text(entry.get("isrc"), 32),
                recording_id=parse_optional_text(entry.get("recording_mbid"), 64),
                rank=rank,
            )
        )

    return (
        FieldCandidate(
            field=TOP_TRACKS_FIELD,
            source=producing_source(source),
            licence=parse_licence(),
            candidates=candidates,
            attribution=parse_attribution(document),
        ),
        None,
    )


def parse_section_entries(section: Any) -> list[Mapping[str, Any]]:
    if section is None:
        return []
    if not isinstance(section, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)
    results = section.get("results")
    if results is None:
        return []
    if not isinstance(results, list):
        raise DocumentError(ErrorCode.resolution_failed, False)
    return results[:DISCOVERY_CANDIDATE_LIMIT]


def require_text(entry: Any, member: str) -> str:
    if not isinstance(entry, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)
    value = entry.get(member)
    if not isinstance(value, str) or not value.strip():
        raise DocumentError(ErrorCode.resolution_failed, False)
    return value.strip()


def parse_artist_names(value: Any) -> list[str] | None:
    if value is None:
        return None
    if not isinstance(value, list):
        raise DocumentError(ErrorCode.resolution_failed, False)

    names = []
    for item in value:
        if not isinstance(item, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)
        name = item.get("name")
        if not isinstance(name, str) or not name.strip():
            raise DocumentError(ErrorCode.resolution_failed, False)
        names.append(name.strip())
    return names or None


def parse_release_name(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)
    name = value.get("name")
    if not isinstance(name, str) or not name.strip():
        raise DocumentError(ErrorCode.resolution_failed, False)
    return name.strip()


def parse_optional_text(value: Any, limit: int) -> str | None:
    if not isinstance(value, str):
        return None
    text = value.strip()
    if not text or len(text) > limit:
        return None
    return text


def parse_duration_ms(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int) and value >= 0:
        return value * 1000
    if isinstance(value, float) and value >= 0:
        return round(value * 1000)
    return None


def parse_largest_thumbnail(thumbnails: Any) -> ImageDescriptor | None:
    if not isinstance(thumbnails, list):
        return None

    for thumbnail in reversed(thumbnails):
        if not isinstance(thumbnail, Mapping):
            continue
        url = thumbnail.get("url")
        if not isinstance(url, str) or not url.startswith(("http://", "https://")):
            continue
        return ImageDescriptor(
            role=WIDE_IMAGE_ROLE,
            reference=full_size_artwork_url(url),
            width=parse_dimension(thumbnail.get("width")),
            height=parse_dimension(thumbnail.get("height")),
        )
    return None


def parse_dimension(value: Any) -> int | None:
    if isinstance(value, int) and not isinstance(value, bool) and value >= 1:
        return value
    return None


def parse_attribution(document: Mapping[str, Any]) -> Attribution | None:
    name = document.get("name")
    if not isinstance(name, str) or not name.strip():
        return None
    return Attribution(text=name)


def producing_source(source: SourceIdentity) -> CandidateSource:
    return CandidateSource(
        provider_id=source.provider_id,
        external_id=source.external_id,
        match_basis=MatchBasis.identifier,
    )


def parse_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=YOUTUBE_MUSIC_TERMS_ID,
        name=YOUTUBE_MUSIC_TERMS_NAME,
        url=YOUTUBE_MUSIC_TERMS_URL,
        attribution_required=True,
        redistribution_allowed=False,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        terms_basis=YOUTUBE_MUSIC_TERMS_BASIS,
        uncertainty=True,
    )
