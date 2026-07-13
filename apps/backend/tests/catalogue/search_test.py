import unittest
from uuid import UUID

from app.catalogue.application import SearchCatalogue
from app.catalogue.domain import CatalogueResult, ProviderId, Track
from app.errors.search import SearchUnavailableError


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


class FailingCatalogueSearchProvider:
    def __init__(self, provider: ProviderId) -> None:
        self.provider = provider

    def search(self, query: str, limit: int) -> list[CatalogueResult]:
        raise RuntimeError("provider is unavailable")


class FakeTrackIdentityResolver:
    def __init__(self) -> None:
        self.candidates: list[CatalogueResult] = []
        self._track_ids = {
            "abc123": UUID("00000000-0000-0000-0000-000000000001"),
            "song-456": UUID("00000000-0000-0000-0000-000000000002"),
            "third-789": UUID("00000000-0000-0000-0000-000000000003"),
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

    def test_deduplicates_by_lumen_id_and_keeps_first_result(self) -> None:
        first_candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        duplicate_candidate = CatalogueResult(
            provider=ProviderId.NAVIDROME,
            external_id="abc123",
            title="Instant Crush (alternate metadata)",
            artists=("Daft Punk",),
            duration_seconds=338,
        )
        first_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.YOUTUBE_MUSIC,
            candidates=[first_candidate],
        )
        second_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.NAVIDROME,
            candidates=[duplicate_candidate],
        )
        identity_resolver = FakeTrackIdentityResolver()
        service = SearchCatalogue(
            identity_resolver=identity_resolver,
            providers=(first_provider, second_provider),
        )

        tracks = service.search("Daft Punk", limit=5)

        self.assertEqual(len(tracks), 1)
        self.assertEqual(tracks[0].id, UUID("00000000-0000-0000-0000-000000000001"))
        self.assertEqual(tracks[0].title, "Instant Crush")
        self.assertEqual(
            identity_resolver.candidates,
            [first_candidate, duplicate_candidate],
        )

    def test_applies_limit_after_processing_all_provider_candidates(self) -> None:
        first_candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            title="Instant Crush",
            artists=("Daft Punk",),
            duration_seconds=337,
        )
        second_candidate = CatalogueResult(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="song-456",
            title="Get Lucky",
            artists=("Daft Punk",),
            duration_seconds=369,
        )
        third_candidate = CatalogueResult(
            provider=ProviderId.NAVIDROME,
            external_id="third-789",
            title="Around the World",
            artists=("Daft Punk",),
            duration_seconds=429,
        )
        first_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.YOUTUBE_MUSIC,
            candidates=[first_candidate, second_candidate],
        )
        second_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.NAVIDROME,
            candidates=[third_candidate],
        )
        identity_resolver = FakeTrackIdentityResolver()
        service = SearchCatalogue(
            identity_resolver=identity_resolver,
            providers=(first_provider, second_provider),
        )

        tracks = service.search("Daft Punk", limit=2)

        self.assertEqual(
            [track.id for track in tracks],
            [
                UUID("00000000-0000-0000-0000-000000000001"),
                UUID("00000000-0000-0000-0000-000000000002"),
            ],
        )
        self.assertEqual(
            identity_resolver.candidates,
            [first_candidate, second_candidate, third_candidate],
        )

    def test_returns_successful_results_when_one_provider_fails(self) -> None:
        candidate = CatalogueResult(
            provider=ProviderId.NAVIDROME,
            external_id="song-456",
            title="Get Lucky",
            artists=("Daft Punk",),
            duration_seconds=369,
        )
        failing_provider = FailingCatalogueSearchProvider(ProviderId.YOUTUBE_MUSIC)
        successful_provider = FakeCatalogueSearchProvider(
            provider=ProviderId.NAVIDROME,
            candidates=[candidate],
        )
        identity_resolver = FakeTrackIdentityResolver()
        service = SearchCatalogue(
            identity_resolver=identity_resolver,
            providers=(failing_provider, successful_provider),
        )

        tracks = service.search("Daft Punk", limit=5)

        self.assertEqual(
            [track.id for track in tracks],
            [UUID("00000000-0000-0000-0000-000000000002")],
        )
        self.assertEqual(successful_provider.calls, [("Daft Punk", 5)])

    def test_raises_search_unavailable_when_all_providers_fail(self) -> None:
        service = SearchCatalogue(
            identity_resolver=FakeTrackIdentityResolver(),
            providers=(
                FailingCatalogueSearchProvider(ProviderId.YOUTUBE_MUSIC),
                FailingCatalogueSearchProvider(ProviderId.NAVIDROME),
            ),
        )

        with self.assertRaises(SearchUnavailableError):
            service.search("Daft Punk", limit=5)

    def test_empty_successful_provider_is_not_search_failure(self) -> None:
        service = SearchCatalogue(
            identity_resolver=FakeTrackIdentityResolver(),
            providers=(
                FakeCatalogueSearchProvider(
                    provider=ProviderId.YOUTUBE_MUSIC,
                    candidates=[],
                ),
            ),
        )

        self.assertEqual(service.search("Daft Punk", limit=5), [])


if __name__ == "__main__":
    unittest.main()
