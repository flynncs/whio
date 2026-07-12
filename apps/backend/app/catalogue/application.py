from collections.abc import Callable, Sequence
from uuid import UUID, uuid7

from app.catalogue.domain import CatalogueResult, SourceIdentity, Track
from app.catalogue.ports import CatalogueGateway, TrackIdentityResolver, TrackRepository


class TrackIdentityService:
    def __init__(
        self,
        repository: TrackRepository,
        id_factory: Callable[[], UUID] = uuid7,
    ) -> None:
        self._repository = repository
        self._id_factory = id_factory

    def resolve_candidate(self, candidate: CatalogueResult) -> Track:
        source_identity = SourceIdentity(
            provider=candidate.provider,
            external_id=candidate.external_id,
        )

        existing_track = self._repository.find_track_by_source(source_identity)
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
        self._repository.attach_source(track.id, source_identity)
        return track


class SearchCatalogue:
    def __init__(
        self,
        identity_resolver: TrackIdentityResolver,
        providers: Sequence[CatalogueGateway],
    ) -> None:
        self._identity_resolver = identity_resolver
        self._providers = tuple(providers)

    def search(self, query: str, limit: int) -> list[Track]:
        tracks: list[Track] = []

        for provider in self._providers:
            candidates = provider.search(query, limit)
            for candidate in candidates:
                tracks.append(self._identity_resolver.resolve_candidate(candidate))

        return tracks
