import unittest
from pathlib import Path
from unittest.mock import Mock

import yaml
from fastapi.testclient import TestClient

from app.api.catalogue import get_catalogue
from app.api.playback import get_playback
from app.generated.resolver_v1 import (
    CapabilitiesResponse,
    CatalogueSearchResponse,
    ErrorResponse,
    HealthResponse,
    PlaybackResolveResponse,
    ReadyResponse,
)
from app.main import app
from app.youtube.catalogue import YouTubeMusicCatalogue
from app.youtube.playback import YouTubeMusicPlayback

CONTRACT_PATH = Path(__file__).parents[3] / "contracts/resolver-v1.openapi.yaml"


class ContractExamplesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        with CONTRACT_PATH.open(encoding="utf-8") as contract_file:
            cls.contract = yaml.safe_load(contract_file)
        cls.client = TestClient(app)

    def tearDown(self) -> None:
        app.dependency_overrides.clear()

    def request_example(self, path: str) -> object:
        return self.contract["paths"][path]["post"]["requestBody"]["content"][
            "application/json"
        ]["example"]

    def response_example(self, path: str) -> object:
        method = "get" if path.startswith("/health/") else "post"
        if path == "/v1/capabilities":
            method = "get"
        return self.contract["paths"][path][method]["responses"]["200"][
            "content"
        ]["application/json"]["example"]

    def test_success_examples_validate_against_generated_models(self) -> None:
        for path, model in [
            ("/health/live", HealthResponse),
            ("/health/ready", ReadyResponse),
            ("/v1/capabilities", CapabilitiesResponse),
            ("/v1/catalogue/search", CatalogueSearchResponse),
            ("/v1/playback/resolve", PlaybackResolveResponse),
        ]:
            with self.subTest(path=path):
                model.model_validate(self.response_example(path))

    def test_error_examples_validate_against_the_generated_model(self) -> None:
        for name, response in self.contract["components"]["responses"].items():
            with self.subTest(response=name):
                example = response["content"]["application/json"]["example"]
                ErrorResponse.model_validate(example)

    def test_catalogue_route_accepts_and_returns_contract_examples(self) -> None:
        expected = CatalogueSearchResponse.model_validate(
            self.response_example("/v1/catalogue/search")
        )
        catalogue = Mock(spec=YouTubeMusicCatalogue)
        catalogue.search.return_value = expected.results
        app.dependency_overrides[get_catalogue] = lambda: catalogue

        response = self.client.post(
            "/v1/catalogue/search",
            json=self.request_example("/v1/catalogue/search"),
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), expected.model_dump(mode="json"))

    def test_playback_route_accepts_and_returns_contract_examples(self) -> None:
        expected = PlaybackResolveResponse.model_validate(
            self.response_example("/v1/playback/resolve")
        )
        playback = Mock(spec=YouTubeMusicPlayback)
        playback.resolve.return_value = expected
        app.dependency_overrides[get_playback] = lambda: playback

        response = self.client.post(
            "/v1/playback/resolve",
            json=self.request_example("/v1/playback/resolve"),
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json(), expected.model_dump(mode="json"))
