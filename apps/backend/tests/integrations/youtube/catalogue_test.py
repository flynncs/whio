import unittest
from unittest.mock import patch

from app.integrations.youtube.catalogue import (
    normalize_song_result,
    parse_duration,
    search_songs,
)


class YouTubeMusicTests(unittest.TestCase):
    def test_parse_duration_supports_minutes_and_seconds(self) -> None:
        self.assertEqual(parse_duration("5:37"), 337)

    def test_parse_duration_supports_hours(self) -> None:
        self.assertEqual(parse_duration("1:02:03"), 3723)

    def test_parse_duration_returns_none_for_missing_or_invalid_values(self) -> None:
        self.assertIsNone(parse_duration(None))
        self.assertIsNone(parse_duration(""))
        self.assertIsNone(parse_duration("unknown"))

    def test_normalize_song_result_maps_provider_fields(self) -> None:
        result = normalize_song_result(
            {
                "videoId": "abc123",
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "duration": "5:37",
            }
        )

        self.assertEqual(result.provider, "youtube_music")
        self.assertEqual(result.external_id, "abc123")
        self.assertEqual(result.title, "Instant Crush")
        self.assertEqual(result.artists, ["Daft Punk"])
        self.assertEqual(result.duration_seconds, 337)

    @patch("app.integrations.youtube.catalogue.ytmusic.search")
    def test_search_songs_calls_provider_and_normalizes_results(self, search) -> None:
        search.return_value = [
            {
                "videoId": "abc123",
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "duration": "5:37",
            }
        ]

        results = search_songs("Daft Punk", limit=5)

        search.assert_called_once_with("Daft Punk", filter="songs", limit=5)
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0].external_id, "abc123")


if __name__ == "__main__":
    unittest.main()
