from typing import Protocol
from uuid import UUID, uuid7

from app.domain.catalogue import CatalogueSearchResult, Recording, SourceReference

SourceKey = tuple[str, str, UUID | None]


class CatalogueRepository(Protocol):
    def import_candidate(self, candidate: CatalogueSearchResult) -> Recording: ...

    def get_recording(self, recording_id: UUID) -> Recording | None: ...

    def get_sources(self, recording_id: UUID) -> tuple[SourceReference, ...]: ...


class InMemoryCatalogueRepository:
    def __init__(self) -> None:
        self._recordings: dict[UUID, Recording] = {}
        self._sources: dict[UUID, tuple[SourceReference, ...]] = {}
        self._recording_ids_by_source: dict[SourceKey, UUID] = {}

    def import_candidate(self, candidate: CatalogueSearchResult) -> Recording:
        source_key: SourceKey = (candidate.provider, candidate.external_id, None)

        existing_recording_id = self._recording_ids_by_source.get(source_key)

        if existing_recording_id is not None:
            return self._recordings[existing_recording_id]

        recording_id = uuid7()

        recording = Recording(
            id=recording_id,
            title=candidate.title,
            artists=tuple(candidate.artists),
            duration_seconds=candidate.duration_seconds,
            provisional=True,
        )

        source = SourceReference(
            recording_id,
            provider=candidate.provider,
            external_id=candidate.external_id,
            upstream_server_id=None,
        )

        self._recordings[recording_id] = recording
        self._sources[recording_id] = (source,)
        self._recording_ids_by_source[source_key] = recording_id

        return recording

    def get_recording(self, recording_id: UUID) -> Recording | None:
        return self._recordings.get(recording_id)

    def get_sources(self, recording_id: UUID) -> tuple[SourceReference, ...]:
        return self._sources.get(recording_id, ())
