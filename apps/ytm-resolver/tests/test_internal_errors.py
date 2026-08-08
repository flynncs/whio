import unittest
from unittest.mock import Mock

from fastapi.testclient import TestClient

from app.api.catalogue import get_catalogue
from app.main import app
from app.youtube.catalogue import YouTubeMusicCatalogue


class InternalErrorTest(unittest.TestCase):
    def setUp(self) -> None:
        catalogue = Mock(spec=YouTubeMusicCatalogue)
        catalogue.search.side_effect = RuntimeError(
            "database password must not reach the client"
        )
        app.dependency_overrides[get_catalogue] = lambda: catalogue
        self.client = TestClient(app, raise_server_exceptions=False)

    def tearDown(self) -> None:
        app.dependency_overrides.clear()

    def test_unexpected_errors_match_the_contract(self) -> None:
        response = self.client.post(
            "/v1/catalogue/search",
            headers={"X-Request-ID": "request-123"},
            json={"query": "Daft Punk", "limit": 5},
        )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.headers["X-Request-ID"], "request-123")
        self.assertEqual(
            response.json(),
            {
                "code": "internal_error",
                "message": "An unexpected error occurred",
                "request_id": "request-123",
            },
        )
        self.assertNotIn("database password", response.text)
