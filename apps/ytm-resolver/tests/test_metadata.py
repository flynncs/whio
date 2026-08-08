import unittest
from unittest.mock import patch

from fastapi.testclient import TestClient

from app.main import app


class MetadataRoutesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = TestClient(app)

    def test_health_routes_match_the_contract(self) -> None:
        for path, expected_body in [
            ("/health/live", {"status": "ok"}),
            ("/health/ready", {"status": "ready"}),
        ]:
            with self.subTest(path=path):
                response = self.client.get(path)

                self.assertEqual(response.status_code, 200)
                self.assertEqual(response.json(), expected_body)

    def test_capabilities_match_the_contract(self) -> None:
        response = self.client.get("/v1/capabilities")

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
                            "playback.resolve",
                        ],
                    }
                ],
            },
        )

    @patch("app.api.catalogue.YTMusic")
    def test_metadata_routes_do_not_start_youtube(self, ytmusic) -> None:
        for path in ["/health/live", "/health/ready", "/v1/capabilities"]:
            with self.subTest(path=path):
                response = self.client.get(path)

                self.assertEqual(response.status_code, 200)

        ytmusic.assert_not_called()
