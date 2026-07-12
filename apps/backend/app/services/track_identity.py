from collections.abc import Callable
from typing import Protocol
from uuid import UUID, uuid7

from app.domain.catalogue import CatalogueSearchResult, SourceIdentity, Track
from app.domain.catalogue_repository import CatalogueRepository


class TrackIdentityResolver(Protocol):
    def resolve_candidate(
        self,
        candidate: CatalogueSearchResult,
    ) -> Track: ...


class TrackIdentityService:
    def __init__(
        self, repository: CatalogueRepository, id_factory: Callable[[], UUID] = uuid7
    ) -> None:
        self._repository = repository
        self._id_factory = id_factory

    def resolve_candidate(self, candidate: CatalogueSearchResult) -> Track:
        source_identity = SourceIdentity(
            provider=candidate.provider,
            external_id=candidate.external_id,
        )

        existing_track = self._repository.find_track_by_source(source=source_identity)

        if existing_track is not None:
            return existing_track

        track = Track(
            id=self._id_factory(),
            title=candidate.title,
            artists=tuple(candidate.artists),
            duration_seconds=candidate.duration_seconds,
            provisional=True,
        )

        self._repository.add_track(track)
        self._repository.attach_source(track_id=track.id, source=source_identity)

        return track
