import unittest
from unittest.mock import Mock

from fastapi.testclient import TestClient

from app.api.catalogue import get_catalogue
from app.errors import ProviderUnavailableError, SourceNotFoundError
from app.generated.resolver_v1 import (
    CatalogueAlbum,
    CatalogueAlbumCandidate,
    CatalogueAlbumSearchResponse,
    CatalogueArtist,
    CatalogueArtistAlbumsResponse,
    CatalogueArtistCandidate,
    CatalogueArtistSearchResponse,
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

    def test_capabilities_advertise_album_artist_search_and_artist_fetch(self) -> None:
        response = self.client.get("/v1/capabilities")
        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["contract_version"], "1")
        self.assertTrue(
            {
                "catalogue.album-search",
                "catalogue.artist-search",
                "catalogue.artist",
            }.issubset(set(response.json()["providers"][0]["capabilities"]))
        )

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

    def test_album_search_matches_the_contract(self) -> None:
        self.catalogue.search_albums.return_value = CatalogueAlbumSearchResponse(
            results=[
                CatalogueAlbumCandidate(
                    source=SourceIdentity(provider_id="youtube_music", external_id="MPREb_album"),
                    title="SOUR",
                    artists=[CatalogueArtist(name="Olivia Rodrigo")],
                    year=2021,
                    release_type="Album",
                )
            ],
            next_offset=25,
        )
        response = self.client.post(
            "/v1/catalogue/album-search",
            json={"query": "SOUR", "limit": 5, "offset": 20},
        )
        self.assertEqual(response.status_code, 200)
        self.catalogue.search_albums.assert_called_once_with("SOUR", 5, 20)
        self.assertEqual(response.json()["next_offset"], 25)
        self.assertEqual(response.json()["results"][0]["title"], "SOUR")

    def test_artist_search_and_artist_fetch_match_the_contract(self) -> None:
        self.catalogue.search_artists.return_value = CatalogueArtistSearchResponse(
            results=[
                CatalogueArtistCandidate(
                    source=SourceIdentity(provider_id="youtube_music", external_id="UC1"),
                    name="Beyoncé",
                )
            ]
        )
        search = self.client.post(
            "/v1/catalogue/artist-search", json={"query": "beyonce", "limit": 5}
        )
        self.assertEqual(search.status_code, 200)
        self.catalogue.search_artists.assert_called_once_with("beyonce", 5, 0)
        self.assertEqual(search.json()["results"][0]["name"], "Beyoncé")

        self.catalogue.fetch_artist.return_value = CatalogueArtistAlbumsResponse(
            results=[
                CatalogueAlbumCandidate(
                    source=SourceIdentity(provider_id="youtube_music", external_id="MPREb_album"),
                    title="SOUR",
                    artists=[CatalogueArtist(name="Olivia Rodrigo")],
                )
            ]
        )
        fetched = self.client.post(
            "/v1/catalogue/artist",
            json={"source": {"provider_id": "youtube_music", "external_id": "UC1"}},
        )
        self.assertEqual(fetched.status_code, 200)
        self.catalogue.fetch_artist.assert_called_once()
        self.assertEqual(fetched.json()["results"][0]["source"]["external_id"], "MPREb_album")

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

    def test_missing_album_source_matches_the_contract(self) -> None:
        self.catalogue.fetch_album.side_effect = SourceNotFoundError(
            "private provider details"
        )

        response = self.client.post(
            "/v1/catalogue/album",
            headers={"X-Request-ID": "request-456"},
            json={
                "source": {
                    "provider_id": "youtube_music",
                    "external_id": "30ce80xVHL4",
                }
            },
        )

        self.assertEqual(response.status_code, 404)
        self.assertEqual(response.headers["X-Request-ID"], "request-456")
        self.assertEqual(
            response.json(),
            {
                "code": "source_not_found",
                "message": "The source is unavailable",
                "request_id": "request-456",
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
