import unittest
from uuid import UUID

import httpx
from fastapi import FastAPI, Request
from starlette.responses import Response

from app.delivery.http.errors import lumen_error_handler, unexpected_error_handler
from app.errors import LumenError, TrackNotFoundError
from app.main import app as main_app


class ErrorHandlerTests(unittest.IsolatedAsyncioTestCase):
    async def test_lumen_error_handler_returns_safe_error_payload(self) -> None:
        test_app = FastAPI()
        test_app.add_exception_handler(LumenError, lumen_error_handler)

        @test_app.get("/tracks/{track_id}")
        def get_track(track_id: UUID):
            raise TrackNotFoundError(track_id)

        transport = httpx.ASGITransport(app=test_app)
        async with httpx.AsyncClient(
            transport=transport,
            base_url="http://test",
        ) as client:
            response = await client.get("/tracks/00000000-0000-0000-0000-000000000001")

        self.assertEqual(response.status_code, 404)
        self.assertEqual(
            response.json(),
            {
                "code": "track_not_found",
                "message": "Track not found",
            },
        )

    async def test_unexpected_error_handler_returns_safe_payload_with_request_id(
        self,
    ) -> None:
        test_app = FastAPI()
        test_app.add_exception_handler(Exception, unexpected_error_handler)

        @test_app.middleware("http")
        async def add_request_id(request: Request, call_next) -> Response:
            request_id = request.headers.get("X-Request-ID", "test-request-id")
            request.state.request_id = request_id
            response = await call_next(request)
            response.headers["X-Request-ID"] = request_id
            return response

        @test_app.get("/unexpected")
        def unexpected() -> None:
            raise RuntimeError("database password should not be exposed")

        transport = httpx.ASGITransport(
            app=test_app,
            raise_app_exceptions=False,
        )
        async with httpx.AsyncClient(
            transport=transport,
            base_url="http://test",
        ) as client:
            response = await client.get(
                "/unexpected",
                headers={"X-Request-ID": "request-123"},
            )

        self.assertEqual(response.status_code, 500)
        self.assertEqual(response.headers["X-Request-ID"], "request-123")
        self.assertEqual(
            response.json(),
            {
                "code": "internal_error",
                "message": "Internal server error",
                "request_id": "request-123",
            },
        )

    def test_main_registers_specific_and_unexpected_handlers(self) -> None:
        self.assertIs(main_app.exception_handlers[LumenError], lumen_error_handler)
        self.assertIs(
            main_app.exception_handlers[Exception],
            unexpected_error_handler,
        )


if __name__ == "__main__":
    unittest.main()
