from typing import Protocol
from uuid import UUID

from app.catalogue.domain import CatalogueResult, ProviderId, SourceIdentity, Track


class TrackRepository(Protocol):
    def add_track(self, track: Track) -> None: ...

    def find_track_by_source(self, source: SourceIdentity) -> Track | None: ...

    def get_track(self, track_id: UUID) -> Track | None: ...

    def get_sources(self, track_id: UUID) -> tuple[SourceIdentity, ...]: ...

    def attach_source(self, track_id: UUID, source: SourceIdentity) -> None: ...


class CatalogueGateway(Protocol):
    provider: ProviderId

    def search(self, query: str, limit: int) -> list[CatalogueResult]: ...


class TrackIdentityResolver(Protocol):
    def resolve_candidate(self, candidate: CatalogueResult) -> Track: ...
