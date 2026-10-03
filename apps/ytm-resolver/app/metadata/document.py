from dataclasses import dataclass
from datetime import datetime
from typing import Protocol, TypeVar

from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    ImageDescriptor,
    LicenceDescriptor,
    MetricDescriptor,
    SourceIdentity,
)

BIOGRAPHY_CONTENT_LIMIT_BYTES = 65536
BIOGRAPHY_SUMMARY_LIMIT_BYTES = 4096
FIELD_TOO_LARGE_MESSAGE = "field_too_large"


@dataclass(frozen=True)
class Candidate:
    source: CandidateSource
    name: str
    artists: list[str] | None = None
    release_name: str | None = None
    duration_ms: int | None = None
    isrc: str | None = None
    recording_id: str | None = None
    rank: int | None = None
    score: float | None = None


@dataclass(frozen=True)
class FieldCandidate:
    field: str
    source: CandidateSource
    licence: LicenceDescriptor
    text: str | None = None
    reference: str | None = None
    image: ImageDescriptor | None = None
    candidates: list[Candidate] | None = None
    language: str | None = None
    rank: int | None = None
    score: float | None = None
    metric: MetricDescriptor | None = None
    count: int | None = None
    ranking_scope: str | None = None
    attribution: Attribution | None = None


@dataclass(frozen=True)
class FieldError:
    field: str
    code: ErrorCode
    retryable: bool
    message: str | None = None


@dataclass(frozen=True)
class ArtistDocument:
    address: SourceIdentity
    retrieved_at: datetime
    candidates: list[FieldCandidate]
    errors: list[FieldError]


class DocumentError(Exception):
    code: ErrorCode
    retryable: bool
    retry_after_seconds: float | None

    def __init__(
        self,
        code: ErrorCode,
        retryable: bool,
        retry_after_seconds: float | None = None,
    ) -> None:
        super().__init__(code.value)
        self.code = code
        self.retryable = retryable
        self.retry_after_seconds = retry_after_seconds


class FieldCarrier(Protocol):
    field: str


FieldCarrierT = TypeVar("FieldCarrierT", bound=FieldCarrier)


def select_requested_fields(
    items: list[FieldCarrierT], fields: list[str] | None
) -> list[FieldCarrierT]:
    if fields is None:
        return items
    requested = set(fields)
    return [item for item in items if item.field in requested]


def exceeds_cap(value: str, limit_bytes: int) -> bool:
    return len(value.encode("utf-8")) > limit_bytes
