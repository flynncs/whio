import unittest
from uuid import UUID
from unittest.mock import AsyncMock, patch

import httpx
from fastapi import FastAPI, Response

from app.catalogue.domain import ProviderId
from app.delivery.http.dependencies import get_resolve_track_playback
from app.delivery.http.playback_routes import router as playback_router
from app.playback.domain import ResolvedPlaybackSource


class FakeResolveTrackPlayback:
    def __init__(self) -> None:
        self.track_ids: list[UUID] = []

    async def execute(self, track_id: UUID) -> ResolvedPlaybackSource:
        self.track_ids.append(track_id)
        return ResolvedPlaybackSource(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
            url="https://example.test/audio",
            headers={},
        )


class PlaybackRouteTests(unittest.IsolatedAsyncioTestCase):
    @patch("app.delivery.http.playback_routes.stream_source", new_callable=AsyncMock)
    async def test_stream_resolves_a_lumen_track_id(self, stream_source) -> None:
        track_id = UUID("00000000-0000-0000-0000-000000000001")
        playback = FakeResolveTrackPlayback()
        stream_source.return_value = Response(status_code=206)

        test_app = FastAPI()
        test_app.include_router(playback_router)
        test_app.dependency_overrides[get_resolve_track_playback] = lambda: playback

        transport = httpx.ASGITransport(app=test_app)
        async with httpx.AsyncClient(
            transport=transport,
            base_url="http://test",
        ) as client:
            response = await client.get(
                f"/playback/stream/{track_id}",
                headers={"Range": "bytes=0-1023"},
            )

        self.assertEqual(response.status_code, 206)
        self.assertEqual(playback.track_ids, [track_id])
        resolved = stream_source.await_args.args[0]
        self.assertEqual(resolved.external_id, "abc123")
        self.assertEqual(
            stream_source.await_args.kwargs["range_header"],
            "bytes=0-1023",
        )


if __name__ == "__main__":
    unittest.main()
