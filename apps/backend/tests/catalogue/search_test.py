import unittest
from uuid import UUID

from app.catalogue.application import SearchCatalogue
from app.catalogue.domain import CatalogueResult, ProviderId, Track


class FakeCatalogueSearchProvider:
    def __init__(
        self,
        provider: ProviderId,
        candidates: list[CatalogueResult],
    ) -> None:
        self.provider = provider
        self._candidates = candidates
        self.calls: list[tuple[str, int]] = []

    def search(self, query: str, limit: int) -> list[CatalogueResult]:
        self.calls.append((query, limit))
        return list(self._candidates)


class FakeTrackIdentityResolver:
    def __init__(self) -> None:
        self.candidates: list[CatalogueResult] = []
        self._track_ids = {
            "abc123": UUID("00000000-0000-0000-0000-000000000001"),
            "song-456": UUID("00000000-0000-0000-0000-000000000002"),
        }

    def resolve_candidate(self, candidate: CatalogueResult) -> Track:
        self.candidates.append(candidate)
        return Track(
            id=self._track_ids[candidate.external_id],
            title=candidate.title,
            artists=candidate.artists,
            duration_seconds=candidate.duration_seconds,
        )


class SearchCatalogueTests(unittest.TestCase):
    def test_delegates_candidates_from_multiple_providers(self) -> None:
        youtube_candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        navidrome_candidate = CatalogueResult(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        youtube_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.YOUTUBE_MUSIC,
            candidates=[youtube_candidate],
        )
        navidrome_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.NAVIDROME,
            candidates=[navidrome_candidate],
        )
        identity_resolver = FakeTrackIdentityResolver()
        service = SearchCatalogue(
            identity_resolver=identity_resolver,
            providers=(youtube_provider, navidrome_provider),
        )

        tracks = service.search("Daft Punk", limit=5)

        self.assertEqual(
            identity_resolver.candidates,
            [youtube_candidate, navidrome_candidate],
        )
        self.assertEqual(
            [track.id for track in tracks],
            [
                UUID("00000000-0000-0000-0000-000000000001"),
                UUID("00000000-0000-0000-0000-000000000002"),
            ],
        )
        self.assertEqual(youtube_provider.calls, [("Daft Punk", 5)])
        self.assertEqual(navidrome_provider.calls, [("Daft Punk", 5)])


if __name__ == "__main__":
    unittest.main()
