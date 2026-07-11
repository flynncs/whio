from typing import Protocol
from uuid import UUID, uuid7

from app.domain.catalogue import (
    CatalogueSearchResult,
    Recording,
    SourceIdentity,
    SourceReference,
)


class CatalogueRepository(Protocol):
    def import_candidate(self, candidate: CatalogueSearchResult) -> Recording: ...

    def get_recording(self, recording_id: UUID) -> Recording | None: ...

    def get_sources(self, recording_id: UUID) -> tuple[SourceReference, ...]: ...

    def attach_source(self, recording_id: UUID, source: SourceIdentity) -> None: ...


class InMemoryCatalogueRepository:
    def __init__(self) -> None:
        self._recordings: dict[UUID, Recording] = {}
        self._sources: dict[UUID, tuple[SourceReference, ...]] = {}
        self._recording_ids_by_source: dict[SourceIdentity, UUID] = {}

    def import_candidate(self, candidate: CatalogueSearchResult) -> Recording:
        source_identity = SourceIdentity(
            provider=candidate.provider,
            external_id=candidate.external_id,
        )

        existing_recording_id = self._recording_ids_by_source.get(source_identity)

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

        self._recordings[recording_id] = recording
        self.attach_source(recording_id, source_identity)

        return recording

    def get_recording(self, recording_id: UUID) -> Recording | None:
        return self._recordings.get(recording_id)

    def get_sources(self, recording_id: UUID) -> tuple[SourceReference, ...]:
        return self._sources.get(recording_id, ())

    def attach_source(self, recording_id: UUID, source: SourceIdentity) -> None:
        if recording_id not in self._recordings:
            raise LookupError(f"Recording {recording_id} does not exist")

        existing_id = self._recording_ids_by_source.get(source)

        if existing_id is not None:
            if existing_id != recording_id:
                raise ValueError("Source belongs to another recording")
            return

        reference = SourceReference(recording_id, identity=source)

        self._recording_ids_by_source[source] = recording_id
        self._sources[recording_id] = (*self._sources.get(recording_id, ()), reference)
