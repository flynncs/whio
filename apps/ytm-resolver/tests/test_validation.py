import unittest

from fastapi.testclient import TestClient

from app.main import app


class RequestValidationTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = TestClient(app)

    def test_invalid_requests_match_the_contract(self) -> None:
        for path, body in [
            (
                "/v1/catalogue/search",
                {"query": "", "limit": 5},
            ),
            (
                "/v1/playback/resolve",
                {
                    "source": {
                        "provider_id": "youtube_music",
                        "external_id": "",
                    }
                },
            ),
        ]:
            with self.subTest(path=path):
                response = self.client.post(
                    path,
                    headers={"X-Request-ID": "request-123"},
                    json=body,
                )

                self.assertEqual(response.status_code, 400)
                self.assertEqual(response.headers["X-Request-ID"], "request-123")
                self.assertEqual(
                    response.json(),
                    {
                        "code": "invalid_request",
                        "message": "The request is invalid",
                        "request_id": "request-123",
                    },
                )
                self.assertNotIn("validation", response.text.lower())
