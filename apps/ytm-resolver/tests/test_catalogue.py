import unittest
from unittest.mock import Mock

from requests.exceptions import RequestException
from ytmusicapi.exceptions import YTMusicServerError

from app.errors import ProviderUnavailableError
from app.youtube.catalogue import (
    YouTubeMusicCatalogue,
    parse_duration_ms,
)


class YouTubeMusicCatalogueTest(unittest.TestCase):
    def test_search_translates_provider_failures(self) -> None:
        for failure in [
            YTMusicServerError("private provider details"),
            RequestException("private transport details"),
        ]:
            with self.subTest(failure=type(failure).__name__):
                client = Mock()
                client.search.side_effect = failure

                with self.assertRaises(ProviderUnavailableError):
                    YouTubeMusicCatalogue(client).search("Daft Punk", limit=5)

    def test_search_calls_youtube_music_and_normalizes_results(self) -> None:
        client = Mock()
        client.search.return_value = [
            {
                "videoId": "abc123",
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "duration": "5:37",
            }
        ]

        results = YouTubeMusicCatalogue(client).search("Daft Punk", limit=5)

        client.search.assert_called_once_with(
            "Daft Punk", filter="songs", limit=5
        )
        self.assertEqual(
            results[0].model_dump(mode="json"),
            {
                "source": {
                    "provider_id": "youtube_music",
                    "external_id": "abc123",
                },
                "title": "Instant Crush",
                "artists": ["Daft Punk"],
                "duration_ms": 337000,
            },
        )

    def test_parse_duration_returns_milliseconds(self) -> None:
        for value, expected in [
            ("5:37", 337000),
            ("1:02:03", 3723000),
        ]:
            with self.subTest(value=value):
                self.assertEqual(parse_duration_ms(value), expected)

    def test_parse_duration_returns_none_for_missing_or_invalid_values(self) -> None:
        for value in [None, "", "unknown"]:
            with self.subTest(value=value):
                self.assertIsNone(parse_duration_ms(value))
