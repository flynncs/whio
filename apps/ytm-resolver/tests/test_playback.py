import json
import subprocess
import unittest
from unittest.mock import patch

from app.generated.resolver_v1 import SourceIdentity
from app.youtube.playback import (
    PlaybackResolutionError,
    UnsupportedProviderError,
    YouTubeMusicPlayback,
)


class YouTubeMusicPlaybackTest(unittest.TestCase):
    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_rejects_a_different_provider(self, run) -> None:
        with self.assertRaises(UnsupportedProviderError):
            YouTubeMusicPlayback().resolve(
                SourceIdentity(
                    provider_id="other_provider",
                    external_id="abc123",
                )
            )

        run.assert_not_called()

    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_translates_a_yt_dlp_failure(self, run) -> None:
        run.side_effect = subprocess.CalledProcessError(
            returncode=1,
            cmd=["yt-dlp"],
            stderr="provider details must not reach the API",
        )

        with self.assertRaises(PlaybackResolutionError):
            YouTubeMusicPlayback().resolve(
                SourceIdentity(
                    provider_id="youtube_music",
                    external_id="abc123",
                )
            )

    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_calls_yt_dlp_and_maps_the_contract(self, run) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=["yt-dlp"],
            returncode=0,
            stdout=json.dumps(
                {
                    "url": "https://media.example.test/audio",
                    "http_headers": {"User-Agent": "test"},
                    "acodec": "opus",
                    "abr": 128.5,
                    "duration": 337.25,
                }
            ),
            stderr="",
        )

        resolved = YouTubeMusicPlayback().resolve(
            SourceIdentity(
                provider_id="youtube_music",
                external_id="abc123",
            )
        )

        run.assert_called_once_with(
            [
                "yt-dlp",
                "--ignore-config",
                "--no-playlist",
                "--format",
                "bestaudio[acodec=opus]/bestaudio",
                "--dump-single-json",
                "https://www.youtube.com/watch?v=abc123",
            ],
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        )
        self.assertEqual(
            resolved.model_dump(mode="json"),
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
