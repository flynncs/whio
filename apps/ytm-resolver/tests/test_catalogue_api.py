import unittest
from unittest.mock import Mock

from fastapi.testclient import TestClient

from app.api.catalogue import get_catalogue
from app.errors import ProviderUnavailableError
from app.generated.resolver_v1 import (
    CatalogueAlbum,
    CatalogueArtist,
    CatalogueCandidate,
    CatalogueCandidatesResponse,
    CatalogueSearchResponse,
    SourceIdentity,
)
from app.main import app
from app.youtube.catalogue import YouTubeMusicCatalogue


class CatalogueRoutesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.catalogue = Mock(spec=YouTubeMusicCatalogue)
        self.catalogue.search.return_value = CatalogueSearchResponse(results=[
            CatalogueCandidate(
                source=SourceIdentity(
                    provider_id="youtube_music",
                    external_id="abc123",
                ),
                title="Instant Crush",
                artists=[CatalogueArtist(name="Daft Punk")],
                duration_ms=337000,
            )
        ])
        app.dependency_overrides[get_catalogue] = lambda: self.catalogue
        self.client = TestClient(app)

    def tearDown(self) -> None:
        app.dependency_overrides.clear()

    def test_search_matches_the_contract(self) -> None:
        response = self.client.post(
            "/v1/catalogue/search",
            json={"query": "Daft Punk", "limit": 5},
        )

        self.assertEqual(response.status_code, 200)
        self.catalogue.search.assert_called_once_with("Daft Punk", 5, 0)
        self.assertEqual(
            response.json(),
            {
                "results": [
                    {
                        "source": {
                            "provider_id": "youtube_music",
                            "external_id": "abc123",
                        },
                        "title": "Instant Crush",
                        "artists": [{"name": "Daft Punk"}],
                        "duration_ms": 337000,
                    }
                ]
            },
        )

    def test_fetch_album_matches_the_contract(self) -> None:
        album_source = SourceIdentity(
            provider_id="youtube_music", external_id="MPREb_album"
        )
        self.catalogue.fetch_album.return_value = CatalogueCandidatesResponse(
            results=[
                CatalogueCandidate(
                    source=SourceIdentity(
                        provider_id="youtube_music", external_id="v1"
                    ),
                    title="brutal",
                    artists=[
                        CatalogueArtist(
                            name="Olivia Rodrigo",
                            source=SourceIdentity(
                                provider_id="youtube_music",
                                external_id="artist-1",
                            ),
                        )
                    ],
                    duration_ms=144000,
                    album=CatalogueAlbum(
                        title="SOUR",
                        source=album_source,
                        year=2021,
                        release_type="Album",
                    ),
                )
            ]
        )

        response = self.client.post(
            "/v1/catalogue/album",
            json={
                "source": {
                    "provider_id": "youtube_music",
                    "external_id": "MPREb_album",
                }
            },
        )

        self.assertEqual(response.status_code, 200)
        self.catalogue.fetch_album.assert_called_once()
        self.assertEqual(
            response.json()["results"][0]["album"]["source"]["external_id"],
            "MPREb_album",
        )
        self.assertEqual(
            response.json()["results"][0]["artists"][0]["source"]["external_id"],
            "artist-1",
        )

    def test_provider_failure_matches_the_contract(self) -> None:
        self.catalogue.search.side_effect = ProviderUnavailableError(
            "private provider details"
        )

        response = self.client.post(
            "/v1/catalogue/search",
            headers={"X-Request-ID": "request-123"},
            json={"query": "Daft Punk", "limit": 5},
        )

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.headers["X-Request-ID"], "request-123")
        self.assertEqual(
            response.json(),
            {
                "code": "provider_unavailable",
                "message": "The provider is temporarily unavailable",
                "request_id": "request-123",
            },
        )
        self.assertNotIn("private provider details", response.text)

    def test_search_passes_offset_and_rejects_out_of_bounds_offsets(self) -> None:
        response = self.client.post(
            "/v1/catalogue/search", json={"query": "songs", "limit": 5, "offset": 25}
        )
        self.assertEqual(response.status_code, 200)
        self.catalogue.search.assert_called_once_with("songs", 5, 25)
        for offset in [-1, 1001]:
            response = self.client.post(
                "/v1/catalogue/search",
                json={"query": "songs", "limit": 5, "offset": offset},
            )
            self.assertEqual(response.status_code, 400)
