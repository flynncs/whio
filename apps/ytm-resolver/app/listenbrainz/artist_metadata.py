from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any

from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    LicenceDescriptor,
    MatchBasis,
    MetricDescriptor,
    SourceIdentity,
)
from app.hosts import HostLimiter
from app.listenbrainz.client import (
    INSTANCE_DISCOVERY_ENTRY_LIMIT,
    LISTENBRAINZ_DEFAULT_ALGORITHM,
    LISTENBRAINZ_PROVIDER,
    ListenBrainzClient,
)
from app.metadata.document import (
    ArtistDocument,
    Candidate,
    DocumentError,
    FieldCandidate,
    FieldError,
    select_requested_fields,
)

SIMILAR_FIELD = "similar"
TOP_TRACKS_FIELD = "top_tracks"
METRIC_LISTEN_FIELD = "metric.listen"
METRIC_LISTENER_FIELD = "metric.listener"

SIMILAR_REQUEST_FIELDS = (SIMILAR_FIELD,)
TOP_TRACKS_REQUEST_FIELDS = (TOP_TRACKS_FIELD,)
METRIC_REQUEST_FIELDS = (METRIC_LISTEN_FIELD, METRIC_LISTENER_FIELD)

LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE = (
    "listenbrainz.popularity.top-recordings-for-artist"
)

LISTENBRAINZ_OUTPUT_LICENCE_ID = "listenbrainz.output"
LISTENBRAINZ_OUTPUT_LICENCE_NAME = "ListenBrainz API"
LISTENBRAINZ_OUTPUT_LICENCE_URL = (
    "https://listenbrainz.readthedocs.io/en/latest/users/api/index.html"
)
LISTENBRAINZ_OUTPUT_LICENCE_BASIS = (
    "The ListenBrainz API documents rate limits and a User-Agent but no data "
    "licence; the computed output's redistribution right is unsettled"
)

LISTENBRAINZ_ATTRIBUTION_TEXT = "ListenBrainz"
LISTENBRAINZ_ATTRIBUTION_URL = "https://listenbrainz.org"

METRIC_LISTENS_UNIT = "listens"
METRIC_LISTENERS_UNIT = "listeners"
METRIC_POPULATION = "ListenBrainz users"

SIMILAR_MBID_MEMBER = "artist_mbid"
SIMILAR_NAME_MEMBER = "name"
SIMILAR_SCORE_MEMBER = "score"
SIMILAR_REFERENCE_MEMBER = "reference_mbid"

RECORDING_MBID_MEMBER = "recording_mbid"
RECORDING_NAME_MEMBER = "recording_name"
RECORDING_LISTEN_COUNT_MEMBER = "total_listen_count"
RECORDING_USER_COUNT_MEMBER = "total_user_count"

POPULARITY_MBID_MEMBER = "artist_mbid"
POPULARITY_LISTEN_COUNT_MEMBER = "total_listen_count"
POPULARITY_USER_COUNT_MEMBER = "total_user_count"


@dataclass(frozen=True)
class SimilarArtist:
    artist_mbid: str
    name: str
    score: float | None


@dataclass(frozen=True)
class RecordingPopularity:
    recording_mbid: str
    name: str
    listen_count: int | None
    user_count: int | None


class ListenBrainzArtistMetadata:
    _client: ListenBrainzClient

    def __init__(self, client: ListenBrainzClient) -> None:
        self._client = client

    def fetch_artist_document(
        self,
        source: SourceIdentity,
        language: str | None,
        fields: list[str] | None,
    ) -> ArtistDocument:
        del language

        retrieved_at = datetime.now(UTC)

        candidates: list[FieldCandidate] = []
        errors: list[FieldError] = []

        if wants(fields, SIMILAR_REQUEST_FIELDS):
            candidate, field_errors = self._similar(source)
            if candidate is not None:
                candidates.append(candidate)
            if field_errors is not None:
                errors.extend(field_errors)

        if wants(fields, TOP_TRACKS_REQUEST_FIELDS):
            candidate, field_errors = self._top_tracks(source)
            if candidate is not None:
                candidates.append(candidate)
            if field_errors is not None:
                errors.extend(field_errors)

        if wants(fields, METRIC_REQUEST_FIELDS):
            metric_candidates, field_errors = self._artist_popularity(source)
            candidates.extend(metric_candidates)
            errors.extend(field_errors)

        return ArtistDocument(
            address=source,
            retrieved_at=retrieved_at,
            candidates=select_requested_fields(candidates, fields),
            errors=select_requested_fields(errors, fields),
        )

    def _similar(
        self, source: SourceIdentity
    ) -> tuple[FieldCandidate | None, list[FieldError] | None]:
        try:
            payload = self._client.fetch_similar_artists(source.external_id)
            entries = parse_similar_artists(payload, source.external_id)
        except DocumentError as error:
            return None, [field_error(SIMILAR_FIELD, error)]

        if not entries:
            return None, None

        producing = producing_source(source)
        return (
            FieldCandidate(
                field=SIMILAR_FIELD,
                source=producing,
                licence=output_licence(),
                candidates=[
                    Candidate(
                        source=CandidateSource(
                            provider_id=LISTENBRAINZ_PROVIDER,
                            external_id=entry.artist_mbid,
                            match_basis=MatchBasis.identifier,
                        ),
                        name=entry.name,
                        rank=rank,
                        score=entry.score,
                    )
                    for rank, entry in enumerate(entries)
                ],
                ranking_scope=self._client.algorithm,
                attribution=attribution(),
            ),
            None,
        )

    def _top_tracks(
        self, source: SourceIdentity
    ) -> tuple[FieldCandidate | None, list[FieldError] | None]:
        try:
            payload = self._client.fetch_top_recordings(source.external_id)
            entries = parse_recordings(payload)
        except DocumentError as error:
            return None, [field_error(TOP_TRACKS_FIELD, error)]

        if not entries:
            return None, None

        producing = producing_source(source)
        return (
            FieldCandidate(
                field=TOP_TRACKS_FIELD,
                source=producing,
                licence=output_licence(),
                candidates=[
                    Candidate(
                        source=CandidateSource(
                            provider_id=LISTENBRAINZ_PROVIDER,
                            external_id=entry.recording_mbid,
                            match_basis=MatchBasis.identifier,
                        ),
                        name=entry.name,
                        rank=entry.listen_count,
                        score=(
                            float(entry.user_count)
                            if entry.user_count is not None
                            else None
                        ),
                    )
                    for entry in entries
                ],
                ranking_scope=LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE,
                attribution=attribution(),
            ),
            None,
        )

    def _artist_popularity(
        self, source: SourceIdentity
    ) -> tuple[list[FieldCandidate], list[FieldError]]:
        try:
            payload = self._client.fetch_artist_popularity(source.external_id)
            counts = parse_artist_popularity(payload, source.external_id)
        except DocumentError as error:
            return [], [
                field_error(METRIC_LISTEN_FIELD, error),
                field_error(METRIC_LISTENER_FIELD, error),
            ]

        if counts is None:
            return [], []

        listen_count, user_count = counts
        producing = producing_source(source)
        licence = output_licence()

        candidates: list[FieldCandidate] = []
        if listen_count is not None:
            candidates.append(
                FieldCandidate(
                    field=METRIC_LISTEN_FIELD,
                    source=producing,
                    licence=licence,
                    count=listen_count,
                    metric=MetricDescriptor(
                        unit=METRIC_LISTENS_UNIT,
                        population=METRIC_POPULATION,
                    ),
                )
            )
        if user_count is not None:
            candidates.append(
                FieldCandidate(
                    field=METRIC_LISTENER_FIELD,
                    source=producing,
                    licence=licence,
                    count=user_count,
                    metric=MetricDescriptor(
                        unit=METRIC_LISTENERS_UNIT,
                        population=METRIC_POPULATION,
                    ),
                )
            )
        return candidates, []


def listenbrainz_artist_metadata(
    token: str | None,
    user_agent: str,
    algorithm: str = LISTENBRAINZ_DEFAULT_ALGORITHM,
    *,
    limiter: HostLimiter,
) -> ListenBrainzArtistMetadata:
    return ListenBrainzArtistMetadata(
        ListenBrainzClient(token, user_agent, algorithm, limiter=limiter)
    )


def wants(fields: list[str] | None, targets: Sequence[str]) -> bool:
    if fields is None:
        return True
    return any(field in targets for field in fields)


def parse_similar_artists(payload: Any, mbid: str) -> list[SimilarArtist]:
    if not isinstance(payload, list):
        raise DocumentError(ErrorCode.resolution_failed, False)

    entries: list[SimilarArtist] = []
    for entry in payload[:INSTANCE_DISCOVERY_ENTRY_LIMIT]:
        if not isinstance(entry, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)

        reference = entry.get(SIMILAR_REFERENCE_MEMBER)
        if reference is not None and reference != mbid:
            raise DocumentError(ErrorCode.resolution_failed, False)

        entries.append(
            SimilarArtist(
                artist_mbid=require_text(entry, SIMILAR_MBID_MEMBER),
                name=require_text(entry, SIMILAR_NAME_MEMBER),
                score=optional_score(entry.get(SIMILAR_SCORE_MEMBER)),
            )
        )
    return entries


def parse_recordings(payload: Any) -> list[RecordingPopularity]:
    if not isinstance(payload, list):
        raise DocumentError(ErrorCode.resolution_failed, False)

    entries: list[RecordingPopularity] = []
    for entry in payload:
        if not isinstance(entry, Mapping):
            raise DocumentError(ErrorCode.resolution_failed, False)

        recording_mbid = require_text(entry, RECORDING_MBID_MEMBER)
        name = optional_text(entry.get(RECORDING_NAME_MEMBER))
        if name is None:
            continue

        entries.append(
            RecordingPopularity(
                recording_mbid=recording_mbid,
                name=name,
                listen_count=optional_count(
                    entry.get(RECORDING_LISTEN_COUNT_MEMBER)
                ),
                user_count=optional_count(entry.get(RECORDING_USER_COUNT_MEMBER)),
            )
        )
        if len(entries) >= INSTANCE_DISCOVERY_ENTRY_LIMIT:
            break
    return entries


def parse_artist_popularity(
    payload: Any, mbid: str
) -> tuple[int | None, int | None] | None:
    if not isinstance(payload, list):
        raise DocumentError(ErrorCode.resolution_failed, False)
    if not payload:
        return None
    if not isinstance(payload[0], Mapping):
        raise DocumentError(ErrorCode.resolution_failed, False)
    if require_text(payload[0], POPULARITY_MBID_MEMBER) != mbid:
        raise DocumentError(ErrorCode.resolution_failed, False)

    return (
        optional_count(payload[0].get(POPULARITY_LISTEN_COUNT_MEMBER)),
        optional_count(payload[0].get(POPULARITY_USER_COUNT_MEMBER)),
    )


def require_text(entry: Mapping[str, Any], member: str) -> str:
    value = entry.get(member)
    if not isinstance(value, str) or not value.strip():
        raise DocumentError(ErrorCode.resolution_failed, False)
    return value.strip()


def optional_text(value: Any) -> str | None:
    if value is None:
        return None
    if not isinstance(value, str):
        raise DocumentError(ErrorCode.resolution_failed, False)
    text = value.strip()
    if not text:
        return None
    return text


def optional_count(value: Any) -> int | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise DocumentError(ErrorCode.resolution_failed, False)
    return value


def optional_score(value: Any) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise DocumentError(ErrorCode.resolution_failed, False)
    return float(value)


def producing_source(source: SourceIdentity) -> CandidateSource:
    return CandidateSource(
        provider_id=LISTENBRAINZ_PROVIDER,
        external_id=source.external_id,
        match_basis=MatchBasis.identifier,
    )


def attribution() -> Attribution:
    return Attribution(
        text=LISTENBRAINZ_ATTRIBUTION_TEXT, url=LISTENBRAINZ_ATTRIBUTION_URL
    )


def output_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id=LISTENBRAINZ_OUTPUT_LICENCE_ID,
        name=LISTENBRAINZ_OUTPUT_LICENCE_NAME,
        url=LISTENBRAINZ_OUTPUT_LICENCE_URL,
        attribution_required=True,
        redistribution_allowed=False,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        terms_basis=LISTENBRAINZ_OUTPUT_LICENCE_BASIS,
        uncertainty=True,
    )


def field_error(field: str, error: DocumentError) -> FieldError:
    return FieldError(field=field, code=error.code, retryable=error.retryable)
