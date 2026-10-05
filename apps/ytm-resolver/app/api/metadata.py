from collections.abc import Callable, Mapping, Sequence
from typing import Annotated
from uuid import uuid7

from fastapi import APIRouter, Body
from fastapi.responses import JSONResponse
from ytmusicapi import YTMusic

from app.generated.resolver_v1 import (
    Artist,
    CapabilityId,
    ErrorCode,
    ErrorResponse,
    MetadataArtistDocumentRequest,
    MetadataArtistDocumentResponse,
    MetadataCandidate,
    MetadataFieldCandidate,
    MetadataFieldError,
)
from app.metadata.document import (
    ArtistDocument,
    Candidate,
    DocumentError,
    FieldCandidate,
    FieldError,
)
from app.youtube.artist_metadata import YouTubeMusicArtistMetadata

YOUTUBE_MUSIC_PROVIDER = "youtube_music"
METADATA_ARTIST_DISPLAY = CapabilityId("metadata.artist.display")
METADATA_CAPABILITY_IDS = frozenset({METADATA_ARTIST_DISPLAY.root})

ARTIST_DISPLAY_FIELDS = frozenset(
    {
        "identity.name",
        "identity.mbid",
        "identity.description",
        "biography.summary",
        "biography.content",
        "biography.language",
        "image.portrait",
        "image.wide",
        "page.url",
        "tag",
        "similar",
        "top_tracks",
        "metric.listeners",
        "metric.plays",
        "metric.listen",
        "metric.listener",
    }
)

ERROR_RESPONSES = {
    ErrorCode.invalid_request: (400, "The request is invalid"),
    ErrorCode.source_not_found: (404, "The source is unavailable"),
    ErrorCode.unsupported_provider: (422, "The provider is not supported"),
    ErrorCode.resolution_failed: (502, "The source document could not be resolved"),
    ErrorCode.provider_unavailable: (503, "The provider is temporarily unavailable"),
}

ArtistMetadata = YouTubeMusicArtistMetadata
ArtistMetadataFactory = Callable[[], ArtistMetadata]


def youtube_music_artist_metadata() -> ArtistMetadata:
    return YouTubeMusicArtistMetadata(YTMusic())


def validate_metadata_dispatch(
    sources: Mapping[str, ArtistMetadataFactory],
    capabilities: Mapping[str, Sequence[CapabilityId]],
) -> None:
    for provider_id, advertised in capabilities.items():
        for capability in advertised:
            if (
                capability.root in METADATA_CAPABILITY_IDS
                and provider_id not in sources
            ):
                raise RuntimeError(
                    f"advertised metadata capability has no registered handler: "
                    f"{provider_id} {capability.root}"
                )
    for provider_id in sources:
        if METADATA_ARTIST_DISPLAY not in capabilities.get(provider_id, ()):
            raise RuntimeError(
                f"metadata handler registered without an advertised capability: "
                f"{provider_id}"
            )


def build_metadata_router(
    sources: Mapping[str, ArtistMetadataFactory],
    capabilities: Mapping[str, Sequence[CapabilityId]],
) -> APIRouter:
    validate_metadata_dispatch(sources, capabilities)

    router = APIRouter(prefix="/v1/metadata")

    @router.post(
        "/artist/display",
        response_model=MetadataArtistDocumentResponse,
        response_model_exclude_none=True,
    )
    def fetch_artist_document(
        body: Annotated[MetadataArtistDocumentRequest, Body()],
    ) -> MetadataArtistDocumentResponse | JSONResponse:
        metadata = artist_metadata(sources, body)
        if metadata is None:
            return error_response(
                DocumentError(ErrorCode.unsupported_provider, False)
            )

        fields: list[str] | None = None
        if body.fields is not None:
            fields = [field.root for field in body.fields]
            if any(field not in ARTIST_DISPLAY_FIELDS for field in fields):
                return error_response(DocumentError(ErrorCode.invalid_request, False))

        try:
            document = metadata.fetch_artist_document(
                body.source, body.language, fields
            )
        except DocumentError as error:
            return error_response(error)

        return document_response(document)

    return router


def artist_metadata(
    sources: Mapping[str, ArtistMetadataFactory],
    body: MetadataArtistDocumentRequest,
) -> ArtistMetadata | None:
    factory = sources.get(body.source.provider_id)
    if factory is None:
        return None
    return factory()


def error_response(error: DocumentError) -> JSONResponse:
    status, message = ERROR_RESPONSES[error.code]
    headers = {}
    if error.retry_after_seconds is not None:
        headers["Retry-After"] = str(int(error.retry_after_seconds))

    body = ErrorResponse(
        code=error.code,
        message=message,
        request_id=str(uuid7()),
    )

    return JSONResponse(
        status_code=status,
        content=body.model_dump(mode="json"),
        headers=headers,
    )


def document_response(document: ArtistDocument) -> MetadataArtistDocumentResponse:
    return MetadataArtistDocumentResponse(
        address=document.address,
        source_revision=None,
        retrieved_at=document.retrieved_at,
        candidates=[field_candidate(candidate) for candidate in document.candidates],
        errors=[field_error(error) for error in document.errors],
    )


def field_candidate(candidate: FieldCandidate) -> MetadataFieldCandidate:
    return MetadataFieldCandidate(
        field=candidate.field,
        source=candidate.source,
        text=candidate.text,
        reference=candidate.reference,
        image=candidate.image,
        candidates=(
            [metadata_candidate(item) for item in candidate.candidates]
            if candidate.candidates
            else None
        ),
        language=candidate.language,
        rank=candidate.rank,
        score=candidate.score,
        metric=candidate.metric,
        count=candidate.count,
        ranking_scope=candidate.ranking_scope,
        licence=candidate.licence,
        attribution=candidate.attribution,
    )


def metadata_candidate(candidate: Candidate) -> MetadataCandidate:
    return MetadataCandidate(
        source=candidate.source,
        name=candidate.name,
        artists=(
            [Artist(artist) for artist in candidate.artists]
            if candidate.artists
            else None
        ),
        release_name=candidate.release_name,
        duration_ms=candidate.duration_ms,
        isrc=candidate.isrc,
        recording_id=candidate.recording_id,
        rank=candidate.rank,
        score=candidate.score,
    )


def field_error(error: FieldError) -> MetadataFieldError:
    return MetadataFieldError(
        field=error.field,
        code=error.code,
        retryable=error.retryable,
        message=error.message,
    )
