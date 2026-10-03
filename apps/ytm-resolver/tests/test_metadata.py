import os
import unittest
from unittest.mock import Mock, patch

from fastapi.testclient import TestClient

from app.api.capabilities import build_capability_router
from app.api.metadata import (
    METADATA_ARTIST_DISPLAY,
    build_metadata_router,
)
from app.generated.resolver_v1 import CapabilityId
from app.main import app
from app.metadata_main import build_app as build_metadata_app

AUDIODB_KEY_VARIABLE = "WHIO_RESOLVER_AUDIODB_KEY"


def metadata_client(key: str) -> TestClient:
    with patch.dict(os.environ, {AUDIODB_KEY_VARIABLE: key}):
        return TestClient(build_metadata_app())


class MetadataRoutesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.clients = {
            "ytm": TestClient(app),
            "metadata": metadata_client("configured"),
        }

    def test_health_routes_match_the_contract(self) -> None:
        for entrypoint, client in self.clients.items():
            for path, expected_body in [
                ("/health/live", {"status": "ok"}),
                ("/health/ready", {"status": "ready"}),
            ]:
                with self.subTest(entrypoint=entrypoint, path=path):
                    response = client.get(path)

                    self.assertEqual(response.status_code, 200)
                    self.assertEqual(response.json(), expected_body)

    def test_capabilities_match_the_contract(self) -> None:
        response = self.clients["ytm"].get("/v1/capabilities")

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            response.json(),
            {
                "contract_version": "1",
                "providers": [
                    {
                        "provider_id": "youtube_music",
                        "capabilities": [
                            "catalogue.search",
                            "catalogue.album",
                            "playback.resolve",
                            "metadata.artist.display",
                        ],
                    }
                ],
            },
        )

    def test_metadata_capabilities_match_the_contract(self) -> None:
        response = self.clients["metadata"].get("/v1/capabilities")

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

    def test_metadata_capabilities_without_a_key_are_unavailable(self) -> None:
        response = metadata_client("").get("/v1/capabilities")

        self.assertEqual(response.status_code, 503)
        self.assertEqual(response.json()["code"], "provider_unavailable")

    @patch("app.api.catalogue.YTMusic")
    def test_metadata_routes_do_not_start_youtube(self, ytmusic) -> None:
        for entrypoint, client in self.clients.items():
            for path in ["/health/live", "/health/ready", "/v1/capabilities"]:
                with self.subTest(entrypoint=entrypoint, path=path):
                    response = client.get(path)

                    self.assertEqual(response.status_code, 200)

        ytmusic.assert_not_called()


class MetadataCapabilityRegistryTest(unittest.TestCase):
    def test_advertised_capability_without_a_handler_is_a_build_error(self) -> None:
        with self.assertRaises(RuntimeError):
            build_capability_router(
                {"musicbrainz": (METADATA_ARTIST_DISPLAY,)}, {}
            )

    def test_registered_handler_without_an_advertisement_is_a_build_error(self) -> None:
        with self.assertRaises(RuntimeError):
            build_capability_router(
                {"theaudiodb": (CapabilityId("catalogue.search"),)},
                {"theaudiodb": Mock()},
            )

    def test_metadata_router_rejects_an_advertised_capability_without_a_handler(
        self,
    ) -> None:
        with self.assertRaises(RuntimeError):
            build_metadata_router(
                sources={},
                capabilities={"musicbrainz": (METADATA_ARTIST_DISPLAY,)},
            )

    def test_metadata_router_rejects_a_handler_without_an_advertisement(self) -> None:
        with self.assertRaises(RuntimeError):
            build_metadata_router(
                sources={"theaudiodb": Mock()},
                capabilities={"theaudiodb": (CapabilityId("catalogue.search"),)},
            )
