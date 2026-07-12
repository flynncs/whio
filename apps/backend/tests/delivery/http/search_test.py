import unittest
from uuid import UUID

import httpx
from fastapi import FastAPI

from app.catalogue.domain import Track
from app.delivery.http.catalogue_routes import router as search_router
from app.delivery.http.dependencies import get_search_catalogue


class FakeSearchCatalogue:
    def search(self, query: str, limit: int) -> list[Track]:
        return [
            Track(
                id=UUID("00000000-0000-0000-0000-000000000001"),
                title=query,
                artists=("Daft Punk",),
                duration_seconds=337,
            )
        ][:limit]


class SearchRouteTests(unittest.IsolatedAsyncioTestCase):
    async def test_search_route_can_override_configured_service(self) -> None:
        test_app = FastAPI()
        test_app.include_router(search_router)
        test_app.dependency_overrides[get_search_catalogue] = lambda: (
            FakeSearchCatalogue()
        )

        transport = httpx.ASGITransport(app=test_app)
        async with httpx.AsyncClient(
            transport=transport,
            base_url="http://test",
        ) as client:
            response = await client.get(
                "/search",
                params={"query": "Daft Punk", "limit": 1},
            )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(response.json()["results"][0]["title"], "Daft Punk")


if __name__ == "__main__":
    unittest.main()
