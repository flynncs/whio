import json
import subprocess
import unittest
from unittest.mock import patch

from app.integrations.youtube.playback.resolver import resolve_audio


class YouTubeResolverTests(unittest.TestCase):
    @patch("app.integrations.youtube.playback.resolver.subprocess.run")
    def test_resolve_audio_maps_yt_dlp_output(self, run) -> None:
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

        resolved = resolve_audio("abc123")

        self.assertEqual(resolved.url, "https://example.test/audio")
        self.assertEqual(resolved.headers["User-Agent"], "test")
        self.assertEqual(resolved.codec, "opus")
        self.assertEqual(resolved.bitrate, 128.5)
        self.assertEqual(resolved.duration, 337.25)

        command = run.call_args.args[0]
        self.assertEqual(command[0], "yt-dlp")
        self.assertIn("https://www.youtube.com/watch?v=abc123", command)


if __name__ == "__main__":
    unittest.main()
