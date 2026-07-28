import unittest
from unittest.mock import Mock

from fastapi.testclient import TestClient

from app.api.playback import get_playback
from app.errors import (
    PlaybackResolutionError,
    ProviderUnavailableError,
    UnsupportedProviderError,
)
from app.generated.resolver_v1 import PlaybackResolveResponse
from app.main import app
from app.youtube.playback import YouTubeMusicPlayback


class PlaybackRoutesTest(unittest.TestCase):
    def setUp(self) -> None:
        self.playback = Mock(spec=YouTubeMusicPlayback)
        app.dependency_overrides[get_playback] = lambda: self.playback
        self.client = TestClient(app)

    def tearDown(self) -> None:
        app.dependency_overrides.clear()

    def test_resolve_matches_the_contract(self) -> None:
        self.playback.resolve.return_value = PlaybackResolveResponse.model_validate(
            {
                "url": "https://media.example.test/audio",
                "headers": {"User-Agent": "test"},
                "media": {
                    "codec": "opus",
                    "bitrate_kbps": 128.5,
                    "duration_ms": 337250,
                },
            }
        )

        response = self.client.post(
            "/v1/playback/resolve",
            json={
                "source": {
                    "provider_id": "youtube_music",
                    "external_id": "abc123",
                }
            },
        )

        self.assertEqual(response.status_code, 200)
        self.assertEqual(
            self.playback.resolve.call_args.args[0].model_dump(),
            {
                "provider_id": "youtube_music",
                "external_id": "abc123",
            },
        )
        self.assertEqual(
            response.json(),
            {
                "url": "https://media.example.test/audio",
                "headers": {"User-Agent": "test"},
                "expires_at": None,
                "media": {
                    "content_type": None,
                    "content_length_bytes": None,
                    "codec": "opus",
                    "bitrate_kbps": 128.5,
                    "duration_ms": 337250,
                },
            },
        )

    def test_known_playback_errors_match_the_contract(self) -> None:
        for error, status, code, message in [
            (
                UnsupportedProviderError("other_provider"),
                422,
                "unsupported_provider",
                "The provider is not supported",
            ),
            (
                PlaybackResolutionError("private provider details"),
                502,
                "resolution_failed",
                "Playback could not be resolved",
            ),
            (
                ProviderUnavailableError("private provider details"),
                503,
                "provider_unavailable",
                "The provider is temporarily unavailable",
            ),
        ]:
            with self.subTest(code=code):
                self.playback.resolve.side_effect = error

                response = self.client.post(
                    "/v1/playback/resolve",
                    headers={"X-Request-ID": "request-123"},
                    json={
                        "source": {
                            "provider_id": "youtube_music",
                            "external_id": "abc123",
                        }
                    },
                )

                self.assertEqual(response.status_code, status)
                self.assertEqual(response.headers["X-Request-ID"], "request-123")
                self.assertEqual(
                    response.json(),
                    {
                        "code": code,
                        "message": message,
                        "request_id": "request-123",
                    },
                )
