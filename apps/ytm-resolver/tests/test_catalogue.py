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
            results.results[0].model_dump(mode="json"),
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

    def test_search_pages_beyond_twenty_five_and_trims_upstream_overfetch(self) -> None:
        client = Mock()
        client.search.return_value = [
            {"videoId": str(i), "title": f"Song {i}", "artists": [{"name": "Artist"}]}
            for i in range(60)
        ]
        page = YouTubeMusicCatalogue(client).search("songs", limit=25, offset=25)
        self.assertEqual(
            [song.source.external_id for song in page.results],
            [str(i) for i in range(25, 50)],
        )
        self.assertEqual(page.next_offset, 50)
        client.search.assert_called_once_with("songs", filter="songs", limit=50)

        last = YouTubeMusicCatalogue(client).search("songs", limit=25, offset=50)
        self.assertEqual(len(last.results), 10)
        self.assertIsNone(last.next_offset)

    def test_search_stops_at_the_offset_bound(self) -> None:
        client = Mock()
        client.search.return_value = [
            {"videoId": str(i), "title": "Song", "artists": [{"name": "Artist"}]}
            for i in range(1050)
        ]
        page = YouTubeMusicCatalogue(client).search("songs", limit=25, offset=1000)
        self.assertEqual(len(page.results), 25)
        self.assertIsNone(page.next_offset)

    def test_parse_duration_returns_none_for_missing_or_invalid_values(self) -> None:
        for value in [None, "", "unknown"]:
            with self.subTest(value=value):
                self.assertIsNone(parse_duration_ms(value))
