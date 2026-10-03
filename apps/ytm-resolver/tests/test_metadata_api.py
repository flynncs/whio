import json
import os
import unittest
from datetime import UTC, datetime
from pathlib import Path
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
from app.listenbrainz.artist_metadata import ListenBrainzArtistMetadata
from app.listenbrainz.client import ListenBrainzClient
from app.metadata.document import (
    ArtistDocument,
    DocumentError,
    FieldCandidate,
    FieldError,
)
from app.metadata_main import build_app as build_metadata_app
from app.musicbrainz.artist_metadata import MusicBrainzArtistMetadata
from app.musicbrainz.client import MusicBrainzClient
from app.youtube.artist_metadata import YouTubeMusicArtistMetadata

RETRIEVED_AT = datetime(2026, 10, 1, 12, 0, tzinfo=UTC)
YOUTUBE_MUSIC_CAPABILITIES = [
    "catalogue.search",
    "catalogue.album",
    "playback.resolve",
    "metadata.artist.display",
]
AUDIODB_KEY_VARIABLE = "WHIO_RESOLVER_AUDIODB_KEY"
MUSICBRAINZ_USER_AGENT_VARIABLE = "WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT"
WIKIMEDIA_USER_AGENT_VARIABLE = "WHIO_RESOLVER_WIKIMEDIA_USER_AGENT"
LISTENBRAINZ_TOKEN_VARIABLE = "WHIO_RESOLVER_LISTENBRAINZ_TOKEN"
LISTENBRAINZ_ALGORITHM_VARIABLE = "WHIO_METADATA_ALGORITHM"
MUSICBRAINZ_USER_AGENT = "whio-resolver/1.0 ( https://example.test/contact )"
MUSICBRAINZ_MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"


def metadata_environment(
    audiodb_key: str = "",
    musicbrainz_user_agent: str = "",
    wikimedia_user_agent: str = "",
) -> dict:
    return {
        AUDIODB_KEY_VARIABLE: audiodb_key,
        MUSICBRAINZ_USER_AGENT_VARIABLE: musicbrainz_user_agent,
        WIKIMEDIA_USER_AGENT_VARIABLE: wikimedia_user_agent,
        LISTENBRAINZ_TOKEN_VARIABLE: "",
        LISTENBRAINZ_ALGORITHM_VARIABLE: "",
    }


def metadata_client(key: str, musicbrainz_user_agent: str = "") -> TestClient:
    with patch.dict(
        os.environ, metadata_environment(key, musicbrainz_user_agent)
    ):
        return TestClient(build_metadata_app())


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
        with patch.dict(os.environ, {"WHIO_RESOLVER_AUDIODB_KEY": "configured"}):
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
        with patch.dict(
            os.environ, {MUSICBRAINZ_USER_AGENT_VARIABLE: MUSICBRAINZ_USER_AGENT}
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json={
                    "source": {
                        "provider_id": "musicbrainz",
                        "external_id": MUSICBRAINZ_MBID,
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
        for key in ["", "configured"]:
            with self.subTest(key=key):
                with patch.dict(os.environ, {"WHIO_RESOLVER_AUDIODB_KEY": key}):
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


class MetadataEntrypointTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = metadata_client("configured")
        cls.unconfigured = metadata_client("")

    def request_body(self, provider_id: str, external_id: str = "mbid") -> dict:
        return {
            "source": {"provider_id": provider_id, "external_id": external_id},
            "profile": "artist-display.v1",
        }

    def test_youtube_music_is_unsupported_on_the_metadata_entrypoint(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            json=self.request_body("youtube_music", "UCIaFw5VBEK8qaW6nRpx_qnw"),
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_capabilities_advertise_theaudiodb_only(self) -> None:
        response = self.client.get("/v1/capabilities")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "contract_version": "1",
                "providers": [
                    {
                        "provider_id": "theaudiodb",
                        "capabilities": ["metadata.artist.display"],
                    }
                ],
            },
        )

    def test_capabilities_without_a_key_are_unavailable(self) -> None:
        response = self.unconfigured.get("/v1/capabilities")

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json()["code"], "provider_unavailable")

    def test_theaudiodb_without_a_key_is_unsupported_provider(self) -> None:
        response = self.unconfigured.post(
            "/v1/metadata/artist/display",
            json=self.request_body(
                "theaudiodb", "cc197bad-dc9c-440d-a5b5-d52ba2e14234"
            ),
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_theaudiodb_dispatch_reaches_its_handler(self) -> None:
        source = SourceIdentity(
            provider_id="theaudiodb",
            external_id="cc197bad-dc9c-440d-a5b5-d52ba2e14234",
        )
        integration = Mock()
        integration.fetch_artist_document.return_value = artist_document(source)

        with patch(
            "app.api.metadata.AudioDbArtistMetadata", return_value=integration
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body("theaudiodb", source.external_id),
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["address"]["provider_id"], "theaudiodb")
        integration.fetch_artist_document.assert_called_once_with(
            source, None, None
        )

    def test_catalogue_and_playback_routes_are_not_mounted(self) -> None:
        for path in [
            "/v1/catalogue/search",
            "/v1/catalogue/album",
            "/v1/playback/resolve",
        ]:
            with self.subTest(path=path):
                response = self.client.post(path, json={})

                self.assertIn(response.status_code, (404, 405))

    def test_health_and_request_id_middleware_match_the_contract(self) -> None:
        for path, expected_body in [
            ("/health/live", {"status": "ok"}),
            ("/health/ready", {"status": "ready"}),
        ]:
            with self.subTest(path=path):
                response = self.client.get(
                    path, headers={"X-Request-ID": "metadata-request-id"}
                )

                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json(), expected_body)
                self.assertEqual(
                    response.headers["X-Request-ID"], "metadata-request-id"
                )

    def test_invalid_request_matches_the_contract(self) -> None:
        response = self.client.post(
            "/v1/metadata/artist/display",
            headers={"X-Request-ID": "metadata-request-id"},
            json={"profile": "artist-display.v1"},
        )

        self.assertEqual(response.status_code, 400)
        self.assertEqual(response.headers["X-Request-ID"], "metadata-request-id")
        self.assertEqual(response.json()["code"], "invalid_request")


class MetadataEntrypointMusicBrainzTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = metadata_client("", MUSICBRAINZ_USER_AGENT)
        cls.unconfigured = metadata_client("", "")

    def request_body(self, **members) -> dict:
        body = {
            "source": {
                "provider_id": "musicbrainz",
                "external_id": MUSICBRAINZ_MBID,
            },
            "profile": "artist-display.v1",
        }
        body.update(members)
        return body

    def musicbrainz_document(self, source: SourceIdentity) -> ArtistDocument:
        return ArtistDocument(
            address=source,
            retrieved_at=RETRIEVED_AT,
            candidates=[
                FieldCandidate(
                    field="identity.mbid",
                    source=CandidateSource(
                        provider_id="musicbrainz",
                        external_id=MUSICBRAINZ_MBID,
                        url=f"https://musicbrainz.org/artist/{MUSICBRAINZ_MBID}",
                        match_basis=MatchBasis.identifier,
                    ),
                    licence=LicenceDescriptor(
                        id="musicbrainz.core",
                        name="CC0 1.0 Universal (Public Domain Dedication)",
                        url="https://creativecommons.org/publicdomain/zero/1.0/",
                        attribution_required=False,
                        redistribution_allowed=True,
                        cache_allowed=True,
                        share_alike=False,
                        non_commercial=False,
                        uncertainty=False,
                    ),
                    text=MUSICBRAINZ_MBID,
                )
            ],
            errors=[],
        )

    def integration_for(self, source: SourceIdentity) -> Mock:
        integration = Mock(spec=MusicBrainzArtistMetadata)
        integration.fetch_artist_document.return_value = self.musicbrainz_document(
            source
        )
        return integration

    def test_musicbrainz_is_advertised_when_its_user_agent_is_configured(
        self,
    ) -> None:
        response = self.client.get("/v1/capabilities")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "contract_version": "1",
                "providers": [
                    {
                        "provider_id": "musicbrainz",
                        "capabilities": ["metadata.artist.display"],
                    }
                ],
            },
        )

    def test_both_configured_sources_are_advertised_separately(self) -> None:
        with patch.dict(
            os.environ,
            metadata_environment("configured", MUSICBRAINZ_USER_AGENT),
        ):
            client = TestClient(build_metadata_app())

        response = client.get("/v1/capabilities")

        self.assertEqual(
            response.json(),
            {
                "contract_version": "1",
                "providers": [
                    {
                        "provider_id": "theaudiodb",
                        "capabilities": ["metadata.artist.display"],
                    },
                    {
                        "provider_id": "musicbrainz",
                        "capabilities": ["metadata.artist.display"],
                    },
                ],
            },
        )

    def test_musicbrainz_without_a_user_agent_is_unsupported_provider(self) -> None:
        response = self.unconfigured.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_a_post_without_a_request_id_is_never_a_body_validation_error(
        self,
    ) -> None:
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        integration = self.integration_for(source)

        with patch(
            "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
            return_value=integration,
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=["identity.mbid"]),
            )

        self.assertEqual(response.status_code, 200)
        self.assertIsNotNone(response.headers["X-Request-ID"])
        body = response.json()
        self.assertNotIn("detail", body)
        self.assertNotIn("loc", body)
        self.assertEqual(
            body["address"],
            {
                "provider_id": "musicbrainz",
                "external_id": MUSICBRAINZ_MBID,
            },
        )
        self.assertEqual(
            body["candidates"][0]["field"], "identity.mbid"
        )
        self.assertEqual(body["candidates"][0]["text"], MUSICBRAINZ_MBID)
        self.assertEqual(
            body["candidates"][0]["source"]["match_basis"], "identifier"
        )
        self.assertEqual(
            body["candidates"][0]["licence"]["id"], "musicbrainz.core"
        )
        self.assertEqual(body["errors"], [])
        integration.fetch_artist_document.assert_called_once_with(
            source, None, ["identity.mbid"]
        )

    def test_musicbrainz_dispatch_passes_the_requested_fields_and_language(
        self,
    ) -> None:
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        integration = self.integration_for(source)

        with patch(
            "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
            return_value=integration,
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(
                    language="de",
                    fields=["biography.summary", "image.portrait"],
                ),
            )

        self.assertEqual(response.status_code, 200)
        integration.fetch_artist_document.assert_called_once_with(
            source, "de", ["biography.summary", "image.portrait"]
        )

    def test_document_errors_map_to_the_contract_for_musicbrainz(self) -> None:
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        integration = self.integration_for(source)

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
                integration.fetch_artist_document.side_effect = DocumentError(
                    code, retryable
                )

                with patch(
                    "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
                    return_value=integration,
                ):
                    response = self.client.post(
                        "/v1/metadata/artist/display",
                        json=self.request_body(),
                    )

                self.assertEqual(response.status_code, status)
                self.assertEqual(response.json()["code"], code.value)
                self.assertEqual(response.json()["message"], message)
                self.assertNotIn("candidates", response.json())

    def test_a_supplied_retry_after_reaches_the_musicbrainz_response(
        self,
    ) -> None:
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        integration = self.integration_for(source)
        integration.fetch_artist_document.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True, retry_after_seconds=17
        )

        with patch(
            "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
            return_value=integration,
        ):
            response = self.client.post(
                "/v1/metadata/artist/display", json=self.request_body()
            )

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.headers["Retry-After"], "17")
        self.assertEqual(response.json()["code"], "provider_unavailable")

    def test_per_field_errors_and_candidates_return_together(self) -> None:
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        document = self.musicbrainz_document(source)
        document = ArtistDocument(
            address=document.address,
            retrieved_at=document.retrieved_at,
            candidates=document.candidates,
            errors=[
                FieldError(
                    field="image.portrait",
                    code=ErrorCode.provider_unavailable,
                    retryable=True,
                )
            ],
        )
        integration = self.integration_for(source)
        integration.fetch_artist_document.return_value = document

        with patch(
            "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
            return_value=integration,
        ):
            response = self.client.post(
                "/v1/metadata/artist/display", json=self.request_body()
            )

        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertEqual(
            [candidate["field"] for candidate in body["candidates"]],
            ["identity.mbid"],
        )
        self.assertEqual(
            body["errors"],
            [
                {
                    "field": "image.portrait",
                    "code": "provider_unavailable",
                    "retryable": True,
                }
            ],
        )
        self.assertNotIn("Retry-After", response.headers)

    def test_the_user_agent_sentinel_is_never_echoed(self) -> None:
        sentinel = "whio-resolver/1.0 ( sentinel-user-agent-value )"
        source = SourceIdentity(
            provider_id="musicbrainz", external_id=MUSICBRAINZ_MBID
        )
        integration = self.integration_for(source)

        for failure in [
            None,
            DocumentError(ErrorCode.provider_unavailable, True),
            DocumentError(ErrorCode.resolution_failed, False),
        ]:
            with self.subTest(failure=failure):
                integration.fetch_artist_document.side_effect = failure
                client = metadata_client("", sentinel)

                with patch(
                    "app.musicbrainz.artist_metadata.MusicBrainzArtistMetadata",
                    return_value=integration,
                ):
                    response = client.post(
                        "/v1/metadata/artist/display",
                        json=self.request_body(),
                    )

                self.assertNotIn(sentinel, response.text)
                self.assertNotIn(
                    sentinel, json.dumps(dict(response.headers))
                )

    def test_the_route_serves_the_real_chain_from_controlled_responses(self) -> None:
        fixtures = Path(__file__).parent / "fixtures" / "metadata"

        def fixture(name: str):
            with (fixtures / name).open(encoding="utf-8") as handle:
                return json.load(handle)

        with patch.object(
            MusicBrainzClient,
            "fetch_artist",
            return_value=fixture("controlled_musicbrainz_artist_document.json"),
        ) as fetch_artist, patch.object(
            MusicBrainzClient,
            "fetch_wikidata_entity",
            return_value=fixture("controlled_wikidata_entity.json"),
        ) as fetch_wikidata, patch.object(
            MusicBrainzClient,
            "fetch_commons_imageinfo",
            return_value=fixture("controlled_commons_imageinfo.json"),
        ) as fetch_commons, patch.object(
            MusicBrainzClient,
            "fetch_wikipedia_summary",
            return_value=fixture("controlled_wikipedia_summary.json"),
        ), patch.object(
            MusicBrainzClient,
            "fetch_wikipedia_extract",
            return_value=fixture("controlled_wikipedia_extract.json"),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display", json=self.request_body()
            )

        self.assertEqual(response.status_code, 200)
        body = response.json()
        fetch_artist.assert_called_once_with(MUSICBRAINZ_MBID)
        fetch_wikidata.assert_called_once_with("Q11647")
        fetch_commons.assert_called_once_with("ColdplayWembley.jpg")
        self.assertEqual(body["address"]["external_id"], MUSICBRAINZ_MBID)
        self.assertEqual(
            [candidate["field"] for candidate in body["candidates"]],
            [
                "identity.mbid",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "tag",
                "image.portrait",
                "biography.summary",
                "biography.content",
                "biography.language",
            ],
        )
        self.assertEqual(body["errors"], [])
        fields = {candidate["field"]: candidate for candidate in body["candidates"]}
        self.assertEqual(fields["identity.mbid"]["licence"]["id"], "musicbrainz.core")
        self.assertEqual(fields["tag"]["licence"]["id"], "musicbrainz.supplementary")
        self.assertEqual(
            fields["tag"]["attribution"]["text"], "MusicBrainz"
        )
        self.assertEqual(
            fields["image.portrait"]["source"]["provider_id"], "commons"
        )
        self.assertEqual(
            fields["image.portrait"]["licence"]["id"], "commons:CC BY 4.0"
        )
        self.assertEqual(
            fields["image.portrait"]["attribution"]["text"], "Ricardo"
        )
        self.assertEqual(
            fields["biography.summary"]["licence"]["id"], "wikipedia.cc-by-sa"
        )
        self.assertEqual(
            fields["biography.summary"]["attribution"],
            {"text": "Coldplay", "url": "https://en.wikipedia.org/wiki/Coldplay"},
        )
        self.assertEqual(fields["biography.language"]["language"], "en")

class EntrypointBoundaryTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.clients = {
            "ytm": TestClient(app),
            "metadata": metadata_client("configured"),
        }

    def test_destination_writes_are_not_mounted(self) -> None:
        for entrypoint, client in self.clients.items():
            for path in ["/v1/destination/connect", "/v1/listen/scrobble"]:
                with self.subTest(entrypoint=entrypoint, path=path):
                    response = client.post(path, json={})

                    self.assertEqual(response.status_code, 404)

    def test_ytm_entrypoint_mounts_catalogue_and_playback(self) -> None:
        client = self.clients["ytm"]

        for path in [
            "/v1/catalogue/search",
            "/v1/catalogue/album",
            "/v1/playback/resolve",
        ]:
            with self.subTest(path=path):
                response = client.post(path, json={})

                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.json()["code"], "invalid_request")


LISTENBRAINZ_DEFAULT_ALGORITHM = (
    "session_based_days_7500_session_300_contribution_5_threshold_10"
    "_limit_100_filter_True_skip_30"
)
LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE = (
    "listenbrainz.popularity.top-recordings-for-artist"
)


def listenbrainz_client(
    token: str = "configured-listenbrainz-token",
    algorithm: str = "",
    user_agent: str = MUSICBRAINZ_USER_AGENT,
) -> TestClient:
    with patch.dict(
        os.environ,
        {
            AUDIODB_KEY_VARIABLE: "",
            MUSICBRAINZ_USER_AGENT_VARIABLE: user_agent,
            WIKIMEDIA_USER_AGENT_VARIABLE: "",
            LISTENBRAINZ_TOKEN_VARIABLE: token,
            LISTENBRAINZ_ALGORITHM_VARIABLE: algorithm,
        },
    ):
        return TestClient(build_metadata_app())


def listenbrainz_fixture(name: str):
    path = Path(__file__).parent / "fixtures" / "metadata" / name
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def listenbrainz_responses(
    similar=None,
    top_recordings=None,
    artist_popularity=None,
):
    responses = {
        "fetch_similar_artists": Mock(
            return_value=listenbrainz_fixture(
                "controlled_listenbrainz_similar_artists.json"
            )
        ),
        "fetch_top_recordings": Mock(
            return_value=listenbrainz_fixture(
                "controlled_listenbrainz_top_recordings.json"
            )
        ),
        "fetch_artist_popularity": Mock(
            return_value=listenbrainz_fixture(
                "controlled_listenbrainz_artist_popularity.json"
            )
        ),
    }
    for member, override in (
        ("fetch_similar_artists", similar),
        ("fetch_top_recordings", top_recordings),
        ("fetch_artist_popularity", artist_popularity),
    ):
        if override is not None:
            responses[member] = override
    return responses


class MetadataEntrypointListenBrainzTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = listenbrainz_client()
        cls.unconfigured = listenbrainz_client(token="")

    def request_body(self, **members) -> dict:
        body = {
            "source": {
                "provider_id": "listenbrainz",
                "external_id": MUSICBRAINZ_MBID,
            },
            "profile": "artist-display.v1",
        }
        body.update(members)
        return body

    def all_fields(self) -> list[str]:
        return ["similar", "top_tracks", "metric.listen", "metric.listener"]

    def post(self, client: TestClient, **members) -> object:
        with patch.multiple(ListenBrainzClient, **listenbrainz_responses()):
            return client.post(
                "/v1/metadata/artist/display", json=self.request_body(**members)
            )

    def test_listenbrainz_is_advertised_when_its_token_is_configured(self) -> None:
        response = self.client.get("/v1/capabilities")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json()["providers"],
            [
                {
                    "provider_id": "musicbrainz",
                    "capabilities": ["metadata.artist.display"],
                },
                {
                    "provider_id": "listenbrainz",
                    "capabilities": ["metadata.artist.display"],
                },
            ],
        )

    def test_listenbrainz_without_a_token_is_unsupported_provider(self) -> None:
        response = self.unconfigured.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_listenbrainz_without_a_user_agent_is_unsupported_provider(
        self,
    ) -> None:
        client = listenbrainz_client(user_agent="")

        response = client.post(
            "/v1/metadata/artist/display", json=self.request_body()
        )

        self.assertEqual(response.status_code, 422)
        self.assertEqual(response.json()["code"], "unsupported_provider")

    def test_a_post_without_a_request_id_is_never_a_body_validation_error(
        self,
    ) -> None:
        response = self.post(self.client, fields=self.all_fields())

        self.assertEqual(response.status_code, 200)
        self.assertIsNotNone(response.headers["X-Request-ID"])
        body = response.json()
        self.assertNotIn("detail", body)
        self.assertNotIn("loc", body)
        self.assertEqual(
            body["address"],
            {
                "provider_id": "listenbrainz",
                "external_id": MUSICBRAINZ_MBID,
            },
        )
        self.assertEqual(
            [candidate["field"] for candidate in body["candidates"]],
            self.all_fields(),
        )
        self.assertEqual(body["errors"], [])

    def test_the_route_keeps_each_named_ranking_scope(self) -> None:
        body = self.post(self.client, fields=self.all_fields()).json()
        fields = {candidate["field"]: candidate for candidate in body["candidates"]}

        self.assertEqual(
            fields["similar"]["ranking_scope"], LISTENBRAINZ_DEFAULT_ALGORITHM
        )
        self.assertEqual(
            fields["top_tracks"]["ranking_scope"],
            LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE,
        )
        self.assertNotIn("ranking_scope", fields["metric.listen"])

    def test_the_configured_algorithm_is_recorded_verbatim(self) -> None:
        client = listenbrainz_client(algorithm="pinned-by-config")

        body = self.post(client, fields=["similar"]).json()

        self.assertEqual(
            body["candidates"][0]["ranking_scope"], "pinned-by-config"
        )

    def test_the_route_keeps_the_unsettled_output_licence(self) -> None:
        body = self.post(self.client, fields=self.all_fields()).json()

        for candidate in body["candidates"]:
            with self.subTest(field=candidate["field"]):
                self.assertEqual(candidate["licence"]["id"], "listenbrainz.output")
                self.assertFalse(candidate["licence"]["redistribution_allowed"])
                self.assertTrue(candidate["licence"]["cache_allowed"])
                self.assertTrue(candidate["licence"]["attribution_required"])
                self.assertTrue(candidate["licence"]["uncertainty"])
                self.assertFalse(candidate["licence"]["share_alike"])
                self.assertFalse(candidate["licence"]["non_commercial"])

    def test_the_route_keeps_the_metric_count_and_population_without_period(
        self,
    ) -> None:
        body = self.post(self.client, fields=self.all_fields()).json()
        fields = {candidate["field"]: candidate for candidate in body["candidates"]}
        popularity = listenbrainz_fixture(
            "controlled_listenbrainz_artist_popularity.json"
        )[0]

        self.assertEqual(
            fields["metric.listen"]["count"], popularity["total_listen_count"]
        )
        self.assertEqual(
            fields["metric.listener"]["count"], popularity["total_user_count"]
        )
        self.assertEqual(fields["metric.listen"]["metric"]["unit"], "listens")
        self.assertEqual(fields["metric.listener"]["metric"]["unit"], "listeners")
        self.assertEqual(
            fields["metric.listen"]["metric"]["population"], "ListenBrainz users"
        )
        self.assertNotIn("period", fields["metric.listen"]["metric"])
        self.assertNotIn("period", fields["metric.listener"]["metric"])

    def test_recordings_are_not_promoted_to_the_artist_metric(self) -> None:
        body = self.post(self.client, fields=self.all_fields()).json()
        fields = {candidate["field"]: candidate for candidate in body["candidates"]}
        recordings = listenbrainz_fixture(
            "controlled_listenbrainz_top_recordings.json"
        )

        self.assertEqual(
            [candidate["rank"] for candidate in fields["top_tracks"]["candidates"]],
            [recording["total_listen_count"] for recording in recordings],
        )
        self.assertEqual(
            [candidate["score"] for candidate in fields["top_tracks"]["candidates"]],
            [float(recording["total_user_count"]) for recording in recordings],
        )
        self.assertNotEqual(
            fields["metric.listen"]["count"],
            fields["top_tracks"]["candidates"][0]["rank"],
        )

    def test_similar_candidates_keep_the_provider_order(self) -> None:
        body = self.post(self.client, fields=["similar"]).json()
        entries = listenbrainz_fixture(
            "controlled_listenbrainz_similar_artists.json"
        )
        candidates = body["candidates"][0]["candidates"]

        self.assertEqual(
            [candidate["name"] for candidate in candidates],
            [entry["name"] for entry in entries],
        )
        self.assertEqual(
            [candidate["rank"] for candidate in candidates], [0, 1, 2]
        )
        self.assertEqual(
            [candidate["score"] for candidate in candidates],
            [entry["score"] for entry in entries],
        )

    def test_empty_upstream_lists_are_absence_not_an_error(self) -> None:
        with patch.multiple(
            ListenBrainzClient,
            **listenbrainz_responses(
                similar=Mock(return_value=[]),
                top_recordings=Mock(return_value=[]),
                artist_popularity=Mock(return_value=[]),
            ),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=self.all_fields()),
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["candidates"], [])
        self.assertEqual(response.json()["errors"], [])

    def test_a_failing_labs_call_is_a_per_field_error_with_the_others_kept(
        self,
    ) -> None:
        with patch.multiple(
            ListenBrainzClient,
            **listenbrainz_responses(
                similar=Mock(
                    side_effect=DocumentError(
                        ErrorCode.provider_unavailable, True
                    )
                )
            ),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=self.all_fields()),
            )

        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertEqual(
            [candidate["field"] for candidate in body["candidates"]],
            ["top_tracks", "metric.listen", "metric.listener"],
        )
        self.assertEqual(
            body["errors"],
            [
                {
                    "field": "similar",
                    "code": "provider_unavailable",
                    "retryable": True,
                }
            ],
        )

    def test_the_route_truncates_the_recording_list_at_50_entries(self) -> None:
        entries = listenbrainz_fixture(
            "controlled_listenbrainz_top_recordings_over_limit.json"
        )
        with patch.multiple(
            ListenBrainzClient,
            **listenbrainz_responses(
                top_recordings=Mock(return_value=entries)
            ),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=["top_tracks"]),
            )

        self.assertEqual(len(entries), 51)
        self.assertEqual(
            len(response.json()["candidates"][0]["candidates"]), 50
        )

    def test_the_token_is_never_echoed_by_the_route(self) -> None:
        sentinel = "sentinel-route-listenbrainz-token"
        client = listenbrainz_client(token=sentinel)

        with patch.multiple(
            ListenBrainzClient,
            **listenbrainz_responses(
                similar=Mock(
                    side_effect=DocumentError(
                        ErrorCode.provider_unavailable, True
                    )
                )
            ),
        ):
            response = client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=self.all_fields()),
            )

        self.assertNotIn(sentinel, response.text)
        self.assertNotIn(sentinel, json.dumps(dict(response.headers)))

    def test_a_429_from_one_call_is_a_retryable_per_field_error(self) -> None:
        with patch.multiple(
            ListenBrainzClient,
            **listenbrainz_responses(
                similar=Mock(
                    side_effect=DocumentError(
                        ErrorCode.provider_unavailable,
                        True,
                        retry_after_seconds=9,
                    )
                )
            ),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=self.all_fields()),
            )

        self.assertEqual(response.status_code, 200)
        body = response.json()
        self.assertEqual(
            body["errors"],
            [
                {
                    "field": "similar",
                    "code": "provider_unavailable",
                    "retryable": True,
                }
            ],
        )
        self.assertIn(
            "top_tracks",
            [candidate["field"] for candidate in body["candidates"]],
        )

    def test_a_supplied_retry_after_reaches_the_listenbrainz_response(self) -> None:
        with patch.object(
            ListenBrainzArtistMetadata,
            "fetch_artist_document",
            side_effect=DocumentError(
                ErrorCode.provider_unavailable, True, retry_after_seconds=17
            ),
        ):
            response = self.client.post(
                "/v1/metadata/artist/display",
                json=self.request_body(fields=self.all_fields()),
            )

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.headers["Retry-After"], "17")
        self.assertEqual(response.json()["code"], "provider_unavailable")
