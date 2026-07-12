from typing import Protocol
from uuid import UUID, uuid7

from app.domain.catalogue import (
    CatalogueSearchResult,
    SourceIdentity,
    Track,
)
from app.errors import SourceConflictError, TrackNotFoundError


class CatalogueRepository(Protocol):
    def import_candidate(self, candidate: CatalogueSearchResult) -> Track: ...

    def get_track(self, track_id: UUID) -> Track | None: ...

    def get_sources(self, track_id: UUID) -> tuple[SourceIdentity, ...]: ...

    def attach_source(self, track_id: UUID, source: SourceIdentity) -> None: ...


class InMemoryCatalogueRepository:
    def __init__(self) -> None:
        self._tracks: dict[UUID, Track] = {}
        self._sources: dict[UUID, tuple[SourceIdentity, ...]] = {}
        self._track_ids_by_source: dict[SourceIdentity, UUID] = {}

    def import_candidate(self, candidate: CatalogueSearchResult) -> Track:
        source_identity = SourceIdentity(
            provider=candidate.provider,
            external_id=candidate.external_id,
        )

        existing_track_id = self._track_ids_by_source.get(source_identity)

        if existing_track_id is not None:
            return self._tracks[existing_track_id]

        track_id = uuid7()

        track = Track(
            id=track_id,
            title=candidate.title,
            artists=tuple(candidate.artists),
            duration_seconds=candidate.duration_seconds,
            provisional=True,
        )

        self._tracks[track_id] = track
        self.attach_source(track_id, source_identity)

        return track

    def get_track(self, track_id: UUID) -> Track | None:
        return self._tracks.get(track_id)

    def get_sources(self, track_id: UUID) -> tuple[SourceIdentity, ...]:
        return self._sources.get(track_id, ())

    def attach_source(self, track_id: UUID, source: SourceIdentity) -> None:
        if track_id not in self._tracks:
            raise TrackNotFoundError(track_id)

        existing_id = self._track_ids_by_source.get(source)

        if existing_id is not None:
            if existing_id != track_id:
                raise SourceConflictError()
            return

        self._track_ids_by_source[source] = track_id
        self._sources[track_id] = (*self._sources.get(track_id, ()), source)
