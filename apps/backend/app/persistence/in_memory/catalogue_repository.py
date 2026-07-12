from uuid import UUID

from app.catalogue.domain import SourceIdentity, Track
from app.catalogue.ports import TrackRepository
from app.errors import SourceConflictError, TrackNotFoundError


class InMemoryCatalogueRepository(TrackRepository):
    def __init__(self) -> None:
        self._tracks: dict[UUID, Track] = {}
        self._sources: dict[UUID, tuple[SourceIdentity, ...]] = {}
        self._track_ids_by_source: dict[SourceIdentity, UUID] = {}

    def add_track(self, track: Track) -> None:
        self._tracks[track.id] = track

    def find_track_by_source(self, source: SourceIdentity) -> Track | None:
        track_id = self._track_ids_by_source.get(source)
        if track_id is None:
            return None
        return self._tracks.get(track_id)

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
