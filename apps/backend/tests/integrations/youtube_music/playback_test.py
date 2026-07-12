import json
import subprocess
import unittest
from unittest.mock import patch

from app.catalogue.domain import ProviderId, SourceIdentity
from app.errors import PlaybackProviderMismatchError
from app.integrations.youtube_music.playback import YouTubeMusicPlayback


class YouTubeMusicPlaybackTests(unittest.IsolatedAsyncioTestCase):
    @patch("app.integrations.youtube_music.playback.subprocess.run")
    async def test_resolve_returns_generic_playback_source(self, run) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=["yt-dlp"],
            returncode=0,
            stdout=json.dumps(
                {
                    "url": "https://example.test/audio",
                    "http_headers": {"User-Agent": "test"},
                    "acodec": "opus",
                    "abr": 128.5,
                    "duration": 337.25,
                }
            ),
            stderr="",
        )

        source = SourceIdentity(
            provider=ProviderId.YOUTUBE_MUSIC,
            external_id="abc123",
        )
        resolved = await YouTubeMusicPlayback().resolve(source)

        self.assertEqual(resolved.provider, ProviderId.YOUTUBE_MUSIC)
        self.assertEqual(resolved.external_id, "abc123")
        self.assertEqual(resolved.url, "https://example.test/audio")
        self.assertEqual(resolved.headers["User-Agent"], "test")
        self.assertEqual(resolved.codec, "opus")
        self.assertEqual(resolved.bitrate, 128.5)
        self.assertEqual(resolved.duration, 337.25)

        command = run.call_args.args[0]
        self.assertEqual(command[0], "yt-dlp")
        self.assertIn("https://www.youtube.com/watch?v=abc123", command)

    async def test_resolve_rejects_a_different_provider(self) -> None:
        source = SourceIdentity(
            provider=ProviderId.MONOCHROME,
            external_id="abc123",
        )

        with self.assertRaises(PlaybackProviderMismatchError):
            await YouTubeMusicPlayback().resolve(source)


if __name__ == "__main__":
    unittest.main()
