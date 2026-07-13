import logging
from collections.abc import Callable, Sequence
from uuid import UUID, uuid7

from app.catalogue.domain import CatalogueResult, SourceIdentity, Track
from app.catalogue.ports import CatalogueGateway, TrackIdentityResolver, TrackRepository
from app.errors.search import SearchUnavailableError

logger = logging.getLogger(__name__)


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
        tracks_by_id: dict[UUID, Track] = {}
        successful_providers = 0

        for provider in self._providers:
            try:
                candidates = provider.search(query, limit)
            except Exception:
                logger.exception(
                    "Catalogue provider search failed",
                    extra={"provider": provider.provider.value},
                )
                continue

            for candidate in candidates:
                track = self._identity_resolver.resolve_candidate(candidate)
                tracks_by_id.setdefault(track.id, track)

            successful_providers += 1

        if successful_providers == 0 and self._providers:
            raise SearchUnavailableError()

        return list(tracks_by_id.values())[:limit]
