import unittest
from uuid import UUID

from fastapi.testclient import TestClient

from app.main import app


class RequestIdTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.client = TestClient(app)

    def test_supplied_request_id_is_returned(self) -> None:
        for path in [
            "/health/live",
            "/health/ready",
            "/v1/capabilities",
        ]:
            with self.subTest(path=path):
                response = self.client.get(
                    path,
                    headers={"X-Request-ID": "test-request-id"},
                )

                self.assertEqual(
                    response.headers.get("X-Request-ID"),
                    "test-request-id",
                )

    def test_missing_request_id_is_created(self) -> None:
        response = self.client.get("/health/live")

        request_id = response.headers.get("X-Request-ID")
        self.assertIsNotNone(request_id)
        UUID(request_id)
