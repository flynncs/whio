import unittest
from unittest.mock import Mock

from requests.exceptions import RequestException
from ytmusicapi.exceptions import YTMusicServerError

from app.errors import ProviderUnavailableError
from app.generated.resolver_v1 import SourceIdentity
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

    def test_fetch_album_normalizes_tracks_with_the_album_identity(self) -> None:
        client = Mock()
        client.get_album.return_value = {
            "title": "SOUR",
            "year": 2021,
            "type": "Album",
            "trackCount": 11,
            "artists": [{"name": "Olivia Rodrigo", "id": "artist-1"}],
            "thumbnails": [
                {"url": "https://art.test/sour.jpg", "width": 544, "height": 544}
            ],
            "tracks": [
                {
                    "videoId": "v1",
                    "title": "brutal",
                    "duration_seconds": 144,
                    "artists": [{"name": "Olivia Rodrigo", "id": "artist-1"}],
                },
                {
                    "videoId": "v2",
                    "title": "traitor",
                    "duration_seconds": 229,
                    "artists": [],
                },
            ],
        }
        source = SourceIdentity(
            provider_id="youtube_music", external_id="MPREb_album"
        )

        response = YouTubeMusicCatalogue(client).fetch_album(source)

        client.get_album.assert_called_once_with("MPREb_album")
        album = response.results[0].album
        self.assertEqual(album.title, "SOUR")
        self.assertEqual(album.source.external_id, "MPREb_album")
        self.assertEqual(album.year, 2021)
        self.assertEqual(album.release_type, "Album")
        self.assertEqual(
            response.results[0].artists[0].source.external_id, "artist-1"
        )
        self.assertEqual(response.results[0].duration_ms, 144000)
        self.assertEqual(response.results[1].duration_ms, 229000)
        self.assertEqual(
            response.results[1].artists[0].name,
            "Olivia Rodrigo",
            "a track with no credit falls back to the album's artists",
        )
        self.assertEqual(
            str(response.results[0].artwork_url), "https://art.test/sour.jpg"
        )

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
            results.results[0].model_dump(mode="json", exclude_none=True),
            {
                "source": {
                    "provider_id": "youtube_music",
                    "external_id": "abc123",
                },
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "duration_ms": 337000,
            },
        )

    def test_search_keeps_album_and_largest_artwork(self) -> None:
        client = Mock()
        client.search.return_value = [
            {
                "videoId": "abc123",
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "album": {"name": "Random Access Memories", "id": "album-1"},
                "thumbnails": [
                    {"url": "https://art.test/small.jpg", "width": 60, "height": 60},
                    {"url": "https://art.test/large.jpg", "width": 544, "height": 544},
                ],
            }
        ]

        results = YouTubeMusicCatalogue(client).search("Daft Punk", limit=5)

        self.assertEqual(results.results[0].album.title, "Random Access Memories")
        self.assertEqual(
            results.results[0].album.source.external_id,
            "album-1",
        )
        self.assertEqual(
            str(results.results[0].artwork_url), "https://art.test/large.jpg"
        )

    def test_search_drops_missing_album_and_artwork(self) -> None:
        client = Mock()
        client.search.return_value = [
            {
                "videoId": "abc123",
                "title": "Instant Crush",
                "artists": [{"name": "Daft Punk"}],
                "album": None,
                "thumbnails": [],
            }
        ]

        results = YouTubeMusicCatalogue(client).search("Daft Punk", limit=5)

        self.assertIsNone(results.results[0].album)
        self.assertIsNone(results.results[0].artwork_url)

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
