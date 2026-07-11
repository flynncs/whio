import unittest
from uuid import UUID

import httpx
from fastapi import FastAPI

from app.api.errors import lumen_error_handler
from app.errors import LumenError, RecordingNotFoundError


class ErrorHandlerTests(unittest.IsolatedAsyncioTestCase):
    async def test_lumen_error_handler_returns_safe_error_payload(self) -> None:
        test_app = FastAPI()
        test_app.add_exception_handler(LumenError, lumen_error_handler)

        @test_app.get("/recordings/{recording_id}")
        def get_recording(recording_id: UUID):
            raise RecordingNotFoundError(recording_id)

        transport = httpx.ASGITransport(app=test_app)
        async with httpx.AsyncClient(
            transport=transport,
            base_url="http://test",
        ) as client:
            response = await client.get(
                "/recordings/00000000-0000-0000-0000-000000000001"
            )

        self.assertEqual(response.status_code, 404)
        self.assertEqual(
            response.json(),
            {
                "code": "recording_not_found",
                "message": "Recording not found",
            },
        )


if __name__ == "__main__":
    unittest.main()
