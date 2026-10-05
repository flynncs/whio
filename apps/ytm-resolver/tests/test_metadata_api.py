import unittest
from datetime import UTC, datetime
from unittest.mock import Mock, patch

from fastapi.testclient import TestClient

from app.generated.resolver_v1 import (
    Attribution,
    CandidateSource,
    ErrorCode,
    LicenceDescriptor,
    MatchBasis,
    SourceIdentity,
)
from app.main import app
from app.metadata.document import (
    ArtistDocument,
    DocumentError,
    FieldCandidate,
    FieldError,
)
from app.youtube.artist_metadata import YouTubeMusicArtistMetadata

RETRIEVED_AT = datetime(2026, 10, 1, 12, 0, tzinfo=UTC)
YOUTUBE_MUSIC_CAPABILITIES = [
    "catalogue.search",
    "catalogue.album",
    "playback.resolve",
    "metadata.artist.display",
]
def terms_licence() -> LicenceDescriptor:
    return LicenceDescriptor(
        id="youtube_music",
        name="YouTube Terms of Service",
        url="https://www.youtube.com/t/terms",
        attribution_required=True,
        redistribution_allowed=False,
        cache_allowed=True,
        share_alike=False,
        non_commercial=False,
        uncertainty=True,
    )


def producing_source(source: SourceIdentity) -> CandidateSource:
    return CandidateSource(
        provider_id=source.provider_id,
        external_id=source.external_id,
        match_basis=MatchBasis.identifier,
    )


def artist_document(source: SourceIdentity) -> ArtistDocument:
    return ArtistDocument(
        address=source,
        retrieved_at=RETRIEVED_AT,
        candidates=[
            FieldCandidate(
                field="biography.content",
                source=producing_source(source),
                licence=terms_licence(),
                text="Coldplay are a British rock band.",
                attribution=Attribution(text="Coldplay"),
            )
        ],
        errors=[],
    )


class MetadataArtistDisplayRouteTest(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SourceIdentity(
            provider_id="youtube_music", external_id="UCIaFw5VBEK8qaW6nRpx_qnw"
        )
        self.integration = Mock(spec=YouTubeMusicArtistMetadata)
        self.integration.fetch_artist_document.return_value = artist_document(
            self.source
        )
        self.patcher = patch(
            "app.api.metadata.YouTubeMusicArtistMetadata",
            return_value=self.integration,
        )
        self.patcher.start()
        self.client = TestClient(app)

    def tearDown(self) -> None:
        self.patcher.stop()

    def request_body(self, **members) -> dict:
        body = {
            "source": {
                "provider_id": self.source.provider_id,
                "external_id": self.source.external_id,
            },
            "profile": "artist-display.v1",
        }
        body.update(members)
        return body

    def test_artist_display_matches_the_contract(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json=self.request_body(
                language="en", fields=["biography.content"]
            ),
        )

        self.assertEqual(response.status_code, 200)
        self.integration.fetch_artist_document.assert_called_once_with(
            self.source, "en", ["biography.content"]
        )
        body = response.json()
        self.assertIsNotNone(response.headers["X-Request-ID"])
        self.assertEqual(
            body,
            {
                "address": {
                    "provider_id": "youtube_music",
                    "external_id": "UCIaFw5VBEK8qaW6nRpx_qnw",
                },
                "retrieved_at": "2026-10-01T12:00:00Z",
                "candidates": [
                    {
                        "field": "biography.content",
                        "source": {
                            "provider_id": "youtube_music",
                            "external_id": "UCIaFw5VBEK8qaW6nRpx_qnw",
                            "match_basis": "identifier",
                        },
                        "text": "Coldplay are a British rock band.",
                        "licence": {
                            "id": "youtube_music",
                            "name": "YouTube Terms of Service",
                            "url": "https://www.youtube.com/t/terms",
                            "attribution_required": True,
                            "redistribution_allowed": False,
                            "cache_allowed": True,
                            "share_alike": False,
                            "non_commercial": False,
                            "uncertainty": True,
                        },
                        "attribution": {"text": "Coldplay"},
                    }
                ],
                "errors": [],
            },
        )

    def test_absent_field_filter_requests_every_profile_field_once(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(response.status_code, 200)
        self.integration.fetch_artist_document.assert_called_once_with(
            self.source, None, None
        )

    def test_many_requested_fields_are_one_integration_call(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json=self.request_body(
                fields=[
                    "biography.content",
                    "image.portrait",
                    "similar",
                    "top_tracks",
                ]
            ),
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(self.integration.fetch_artist_document.call_count, 1)

    def test_unknown_requested_field_is_invalid_request(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json=self.request_body(fields=["not_a_profile_field"]),
        )

        self.assertEqual(response.status_code, 400)
        self.integration.fetch_artist_document.assert_not_called()
        self.assertEqual(response.json()["code"], "invalid_request")

    def test_an_invalid_profile_or_language_is_invalid_request(self) -> None:
        for body in [
            self.request_body(profile="other-profile.v1"),
            self.request_body(language="e"),
        ]:
            with self.subTest(body=body):
                response = self.client.post(
                    "/v1/metadata/artist/display", json=body
                )

                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.json()["code"], "invalid_request")

        self.integration.fetch_artist_document.assert_not_called()

    def test_unknown_provider_is_unsupported_provider(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json={
                "source": {"provider_id": "listenbrainz", "external_id": "mbid"},
                "profile": "artist-display.v1",
            },
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")
        self.assertNotIn("listenbrainz", response.text)

    def test_theaudiodb_is_unsupported_on_the_ytm_entrypoint(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json={
                "source": {
                    "provider_id": "theaudiodb",
                    "external_id": "cc197bad-dc9c-440d-a5b5-d52ba2e14234",
                },
                "profile": "artist-display.v1",
            },
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_musicbrainz_is_unsupported_on_the_ytm_entrypoint(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json={
                "source": {
                    "provider_id": "musicbrainz",
                    "external_id": "cc197bad-dc9c-440d-a5b5-d52ba2e14234",
                },
                "profile": "artist-display.v1",
            },
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_document_errors_map_to_the_contract(self) -> None:
        for code, status, message, retryable in [
            (ErrorCode.source_not_found, 404, "The source is unavailable", False),
            (
                ErrorCode.resolution_failed,
                502,
                "The source document could not be resolved",
                False,
            ),
            (
                ErrorCode.provider_unavailable,
                503,
                "The provider is temporarily unavailable",
                True,
            ),
        ]:
            with self.subTest(code=code):
                self.integration.fetch_artist_document.side_effect = DocumentError(
                    code, retryable
                )

                response = self.client.post(
                    "/v1/metadata/artist/display",
                    json=self.request_body(),
                )

                self.assertEqual(response.status_code, status)
                self.assertEqual(response.json()["code"], code.value)
                self.assertEqual(response.json()["message"], message)
                self.assertIsNotNone(response.headers.get("X-Request-ID"))

    def test_retry_after_is_propagated(self) -> None:
        self.integration.fetch_artist_document.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True, retry_after_seconds=7
        )

        response = self.client.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.headers["Retry-After"], "7")
        self.assertEqual(response.json()["code"], "provider_unavailable")

    def test_field_errors_carry_their_message(self) -> None:
        self.integration.fetch_artist_document.return_value = ArtistDocument(
            address=self.source,
            retrieved_at=RETRIEVED_AT,
            candidates=[],
            errors=[
                FieldError(
                    field="biography.content",
                    code=ErrorCode.resolution_failed,
                    retryable=False,
                    message="field_too_large",
                )
            ],
        )

        response = self.client.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(
            response.json()["errors"],
            [
                {
                    "field": "biography.content",
                    "code": "resolution_failed",
                    "retryable": False,
                    "message": "field_too_large",
                }
            ],
        )

    def test_capabilities_advertise_youtube_music_only(self) -> None:
        response = self.client.get("/v1/capabilities")
        self.assertEqual(
            response.json(),
            {
                "contract_version": "1",
                "providers": [
                    {
                        "provider_id": "youtube_music",
                        "capabilities": YOUTUBE_MUSIC_CAPABILITIES,
                    }
                ],
            },
        )



class EntrypointBoundaryTest(unittest.TestCase):
    def test_destination_writes_are_not_mounted(self) -> None:
        client = TestClient(app)
        for path in ["/v1/destination/connect", "/v1/listen/scrobble"]:
            with self.subTest(path=path):
                response = client.post(path, json={})
                self.assertEqual(response.status_code, 404)

    def test_ytm_entrypoint_mounts_catalogue_and_playback(self) -> None:
        client = TestClient(app)
        for path in [
            "/v1/catalogue/search",
            "/v1/catalogue/album",
            "/v1/playback/resolve",
        ]:
            with self.subTest(path=path):
                response = client.post(path, json={})
                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.json()["code"], "invalid_request")
