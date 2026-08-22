import json
import subprocess
import threading
import unittest
from unittest.mock import patch

from app.errors import (
    PlaybackResolutionError,
    ProviderUnavailableError,
    UnsupportedProviderError,
)
from app.generated.resolver_v1 import SourceIdentity
from app.youtube.playback import YouTubeMusicPlayback, YtDlpAudioPayload


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

        self.assertEqual(run.call_count, 2)

    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_translates_unavailable_yt_dlp(self, run) -> None:
        for failure in [
            subprocess.TimeoutExpired(cmd=["yt-dlp"], timeout=30),
            FileNotFoundError("yt-dlp"),
        ]:
            with self.subTest(failure=type(failure).__name__):
                run.side_effect = failure

                with self.assertRaises(ProviderUnavailableError):
                    YouTubeMusicPlayback().resolve(
                        SourceIdentity(
                            provider_id="youtube_music",
                            external_id="abc123",
                        )
                    )

    @patch("app.youtube.playback.requests.get")
    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_calls_yt_dlp_and_maps_the_contract(self, run, get) -> None:
        both_started = threading.Barrier(2)

        def resolve_format(*args, **kwargs):
            both_started.wait(timeout=1)
            return subprocess.CompletedProcess(
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

        run.side_effect = resolve_format
        get.return_value.__enter__.return_value.status_code = 206

        resolved = YouTubeMusicPlayback().resolve(
            SourceIdentity(
                provider_id="youtube_music",
                external_id="abc123",
            )
        )

        self.assertEqual(run.call_count, 2)
        self.assertEqual(
            {call.args[0][4] for call in run.call_args_list},
            {"bestaudio[acodec=opus]", "bestaudio[ext=m4a]"},
        )
        self.assertGreaterEqual(get.call_count, 1)
        for call in get.call_args_list:
            self.assertEqual(call.args[0], "https://media.example.test/audio")
            self.assertEqual(
                call.kwargs,
                {
                    "headers": {"User-Agent": "test", "Range": "bytes=0-"},
                    "stream": True,
                    "allow_redirects": True,
                    "timeout": (5, 10),
                },
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

    @patch("app.youtube.playback._resolve_candidate")
    def test_resolve_returns_when_the_first_candidate_succeeds(
        self,
        resolve_candidate,
    ) -> None:
        slow_started = threading.Event()
        release_slow = threading.Event()
        slow_finished = threading.Event()
        resolution_finished = threading.Event()
        result = []
        failures = []

        payload = YtDlpAudioPayload(
            url="https://media.example.test/audio",
            acodec="opus",
        )

        def resolve_format(video_id, audio_format):
            if audio_format == "bestaudio[acodec=opus]":
                slow_started.set()
                release_slow.wait(timeout=1)
                slow_finished.set()
            return payload, 206

        def resolve() -> None:
            try:
                result.append(
                    YouTubeMusicPlayback().resolve(
                        SourceIdentity(
                            provider_id="youtube_music",
                            external_id="abc123",
                        )
                    )
                )
            except BaseException as error:
                failures.append(error)
            finally:
                resolution_finished.set()

        resolve_candidate.side_effect = resolve_format
        resolver_thread = threading.Thread(target=resolve)
        resolver_thread.start()

        try:
            self.assertTrue(slow_started.wait(timeout=1))
            self.assertTrue(resolution_finished.wait(timeout=1))
            self.assertEqual(len(failures), 0)
            self.assertEqual(str(result[0].url), "https://media.example.test/audio")
        finally:
            release_slow.set()
            self.assertTrue(slow_finished.wait(timeout=1))
            resolver_thread.join(timeout=1)
            self.assertFalse(resolver_thread.is_alive())

    @patch("app.youtube.playback.requests.get")
    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_falls_back_when_the_first_url_is_rejected(
        self,
        run,
        get,
    ) -> None:
        def resolve_format(command, **kwargs):
            is_opus = command[4] == "bestaudio[acodec=opus]"
            return subprocess.CompletedProcess(
                args=["yt-dlp"],
                returncode=0,
                stdout=json.dumps(
                    {
                        "url": (
                            "https://media.example.test/opus"
                            if is_opus
                            else "https://media.example.test/aac"
                        ),
                        "acodec": "opus" if is_opus else "mp4a.40.2",
                    }
                ),
                stderr="",
            )

        def validate_format(url, **kwargs):
            response = unittest.mock.MagicMock()
            response.__enter__.return_value.status_code = (
                403 if url.endswith("/opus") else 206
            )
            return response

        run.side_effect = resolve_format
        get.side_effect = validate_format

        resolved = YouTubeMusicPlayback().resolve(
            SourceIdentity(
                provider_id="youtube_music",
                external_id="abc123",
            )
        )

        self.assertEqual(str(resolved.url), "https://media.example.test/aac")
        self.assertEqual(resolved.media.codec, "mp4a.40.2")
        self.assertEqual(run.call_count, 2)

    @patch("app.youtube.playback.time.sleep")
    @patch("app.youtube.playback.requests.get")
    @patch("app.youtube.playback.subprocess.run")
    def test_resolve_retries_with_fresh_urls_after_a_rejected_pass(
        self,
        run,
        get,
        sleep,
    ) -> None:
        run.return_value = subprocess.CompletedProcess(
            args=["yt-dlp"],
            returncode=0,
            stdout=json.dumps(
                {
                    "url": "https://media.example.test/audio",
                    "acodec": "opus",
                }
            ),
            stderr="",
        )

        first = unittest.mock.MagicMock()
        first.__enter__.return_value.status_code = 403
        second = unittest.mock.MagicMock()
        second.__enter__.return_value.status_code = 403
        accepted = unittest.mock.MagicMock()
        accepted.__enter__.return_value.status_code = 206
        also_accepted = unittest.mock.MagicMock()
        also_accepted.__enter__.return_value.status_code = 206
        get.side_effect = [first, second, accepted, also_accepted]

        resolved = YouTubeMusicPlayback().resolve(
            SourceIdentity(
                provider_id="youtube_music",
                external_id="abc123",
            )
        )

        self.assertEqual(str(resolved.url), "https://media.example.test/audio")
        self.assertEqual(run.call_count, 4)
        sleep.assert_called_once_with(0.2)
