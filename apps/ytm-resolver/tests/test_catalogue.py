import json
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from requests.exceptions import RequestException
from ytmusicapi.exceptions import YTMusicServerError, YTMusicUserError

from app.errors import ProviderUnavailableError, SourceNotFoundError
from app.generated.resolver_v1 import SourceIdentity
from app.youtube.catalogue import (
    YouTubeMusicCatalogue,
    parse_duration_ms,
    parse_positive_number,
)


CATALOGUE_FIXTURES = Path(__file__).parent / "fixtures" / "catalogue"


def load_catalogue_fixture(name: str):
    with (CATALOGUE_FIXTURES / name).open(encoding="utf-8") as handle:
        return json.load(handle)


def album_source() -> SourceIdentity:
    return SourceIdentity(provider_id="youtube_music", external_id="MPREb_ego")


class YouTubeMusicCatalogueTest(unittest.TestCase):
    def test_track_numbers_reject_zero_negative_and_non_integer_values(self) -> None:
        for value in [None, 0, -1, True, "1", 1.5]:
            self.assertIsNone(parse_positive_number(value))
        self.assertEqual(parse_positive_number(1), 1)

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
                    "trackNumber": 1,
                    "discNumber": 2,
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
        self.assertEqual(response.reported_count, 11)
        album = response.results[0].album
        self.assertEqual(album.title, "SOUR")
        self.assertEqual(album.source.external_id, "MPREb_album")
        self.assertEqual(album.year, 2021)
        self.assertEqual(album.release_type, "Album")
        self.assertEqual(str(album.artwork_url), "https://art.test/sour.jpg")
        self.assertEqual(
            response.results[0].artists[0].source.external_id, "artist-1"
        )
        self.assertEqual(response.results[0].track_number, 1)
        self.assertEqual(response.results[0].disc_number, 2)
        self.assertIsNone(response.results[1].track_number)
        self.assertIsNone(response.results[1].disc_number)
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

    def test_album_search_keeps_the_largest_thumbnail(self) -> None:
        client = Mock()
        client.search.return_value = [
            {
                "browseId": "MPREb_album",
                "title": "SOUR",
                "artists": [{"name": "Olivia Rodrigo"}],
                "thumbnails": [
                    {"url": "https://art.test/small.jpg", "width": 60, "height": 60},
                    {"url": "https://art.test/large.jpg", "width": 544, "height": 544},
                ],
            }
        ]

        page = YouTubeMusicCatalogue(client).search_albums("SOUR", 5)

        self.assertEqual(
            str(page.results[0].artwork_url), "https://art.test/large.jpg"
        )

    def test_album_search_without_thumbnails_has_no_artwork(self) -> None:
        client = Mock()
        client.search.return_value = [
            {
                "browseId": "MPREb_album",
                "title": "SOUR",
                "artists": [{"name": "Olivia Rodrigo"}],
            }
        ]

        page = YouTubeMusicCatalogue(client).search_albums("SOUR", 5)

        self.assertIsNone(page.results[0].artwork_url)

    def test_album_search_slices_raw_rows_before_dropping_invalid_rows(self) -> None:
        client = Mock()
        client.search.return_value = [
            {"browseId": "MPREb_0", "title": "Skip", "artists": [{"name": "A"}]},
            {"browseId": "MPREb_1", "title": "The Marías", "artists": [{"name": "Marias", "id": "UC1"}]},
            {"browseId": "not-an-album", "title": "No ID", "artists": [{"name": "A"}]},
            {"browseId": "MPREb_3", "title": "Valid", "artists": [{"name": "B"}]},
        ]
        page = YouTubeMusicCatalogue(client).search_albums("marias", 2, 1)
        client.search.assert_called_once_with("marias", filter="albums", limit=3)
        self.assertEqual([album.title for album in page.results], ["The Marías"])
        self.assertEqual(page.results[0].artists[0].source.external_id, "UC1")
        self.assertEqual(page.next_offset, 3)

    def test_artist_search_drops_rows_without_real_identity_or_name(self) -> None:
        client = Mock()
        client.search.return_value = [
            {"browseId": "UC1", "artist": "Beyoncé"},
            {"browseId": "not-an-artist", "artist": "Missing identity"},
            {"browseId": "UC3", "artist": "  "},
        ]
        page = YouTubeMusicCatalogue(client).search_artists("beyonce", 3)
        client.search.assert_called_once_with("beyonce", filter="artists", limit=3)
        self.assertEqual([artist.name for artist in page.results], ["Beyoncé"])
        self.assertEqual(page.next_offset, 3)

    def test_entity_search_drops_non_mapping_rows(self) -> None:
        client = Mock()
        client.search.side_effect = [
            [
                None,
                {"browseId": "MPREb_album", "title": "SOUR", "artists": [{"name": "Artist"}]},
            ],
            [False, {"browseId": "UC1", "artist": "Beyoncé"}],
        ]

        albums = YouTubeMusicCatalogue(client).search_albums("SOUR", 2)
        artists = YouTubeMusicCatalogue(client).search_artists("beyonce", 2)

        self.assertEqual([album.title for album in albums.results], ["SOUR"])
        self.assertEqual([artist.name for artist in artists.results], ["Beyoncé"])
        self.assertEqual(client.search.call_count, 2)

    def test_artist_fetch_drops_non_mapping_shelf_rows(self) -> None:
        client = Mock()
        client.get_artist.return_value = {
            "albums": {
                "results": [
                    None,
                    {"browseId": "MPREb_album", "title": "SOUR", "artists": [{"name": "Artist"}]},
                ]
            },
            "singles": {
                "results": [
                    "malformed",
                    {"browseId": "MPREb_single", "title": "Single", "artists": [{"name": "Artist"}]},
                ]
            },
        }

        result = YouTubeMusicCatalogue(client).fetch_artist(
            SourceIdentity(provider_id="youtube_music", external_id="UCartist")
        )

        self.assertEqual(
            [album.source.external_id for album in result.results],
            ["MPREb_album", "MPREb_single"],
        )

    def test_artist_fetch_maps_non_mapping_artist_response_to_source_not_found(self) -> None:
        client = Mock()
        client.get_artist.return_value = []

        with self.assertRaises(SourceNotFoundError):
            YouTubeMusicCatalogue(client).fetch_artist(
                SourceIdentity(provider_id="youtube_music", external_id="UCartist")
            )

        client.get_artist_albums.assert_not_called()

    def test_artist_fetch_maps_non_mapping_expanded_shelf_to_provider_error(self) -> None:
        client = Mock()
        client.get_artist.return_value = {
            "albums": {"results": [], "browseId": "VL1", "params": "album-params"}
        }
        client.get_artist_albums.return_value = None

        with self.assertRaises(ProviderUnavailableError):
            YouTubeMusicCatalogue(client).fetch_artist(
                SourceIdentity(provider_id="youtube_music", external_id="UCartist")
            )

    def test_artist_fetch_accepts_a_list_expanded_shelf(self) -> None:
        client = Mock()
        client.get_artist.return_value = {
            "albums": {
                "results": [
                    {"browseId": "MPREb_1", "title": "First", "artists": [{"name": "Artist"}]}
                ],
                "browseId": "VL1",
                "params": "album-params",
            },
        }
        client.get_artist_albums.return_value = [
            {"browseId": "MPREb_1", "title": "Duplicate", "artists": [{"name": "Artist"}]},
            {"browseId": "MPREb_2", "title": "Second", "artists": [{"name": "Artist"}]},
            {"browseId": "MPREb_2", "title": "Duplicate", "artists": [{"name": "Artist"}]},
        ]

        result = YouTubeMusicCatalogue(client).fetch_artist(
            SourceIdentity(provider_id="youtube_music", external_id="UCartist")
        )

        self.assertEqual(
            [album.source.external_id for album in result.results],
            ["MPREb_1", "MPREb_2"],
        )
        self.assertEqual(
            [album.title for album in result.results], ["Duplicate", "Second"]
        )
        client.get_artist_albums.assert_called_once_with("VL1", "album-params")

    def test_artist_fetch_expands_shelves_and_deduplicates_in_order(self) -> None:
        client = Mock()
        client.get_artist.return_value = {
            "albums": {"results": [{"browseId": "MPREb_1", "title": "First", "artists": [{"name": "Artist"}]}], "browseId": "VL1", "params": "album-params"},
            "singles": {"results": [{"browseId": "MPREb_2", "title": "Single", "artists": [{"name": "Artist"}]}], "browseId": "VL2", "params": "single-params"},
        }
        client.get_artist_albums.side_effect = [
            {"results": [
                {"browseId": "MPREb_1", "title": "Duplicate", "artists": [{"name": "Artist"}]},
                {"browseId": "MPREb_3", "title": "Second", "artists": [{"name": "Artist"}]},
            ]},
            {"results": [
                {"browseId": "MPREb_3", "title": "Duplicate single", "artists": [{"name": "Artist"}]},
                {"browseId": "MPREb_4", "title": "Fourth", "artists": [{"name": "Artist"}]},
            ]},
        ]
        result = YouTubeMusicCatalogue(client).fetch_artist(
            SourceIdentity(provider_id="youtube_music", external_id="UCartist")
        )
        self.assertEqual(
            [album.source.external_id for album in result.results],
            ["MPREb_1", "MPREb_3", "MPREb_4"],
        )
        self.assertEqual([album.title for album in result.results], ["Duplicate", "Second", "Fourth"])
        client.get_artist.assert_called_once_with("UCartist")
        self.assertEqual(client.get_artist_albums.call_count, 2)

    def test_artist_fetch_caps_each_shelf_and_total_distinct_identities(self) -> None:
        client = Mock()
        albums = [
            {"browseId": f"MPREb_{index}", "title": f"Album {index}", "artists": [{"name": "Artist"}]}
            for index in range(120)
        ]
        singles = [
            {"browseId": f"MPREb_{index}", "title": f"Single {index}", "artists": [{"name": "Artist"}]}
            for index in range(100, 220)
        ]
        client.get_artist.return_value = {
            "albums": {"results": albums},
            "singles": {"results": singles},
        }
        result = YouTubeMusicCatalogue(client).fetch_artist(
            SourceIdentity(provider_id="youtube_music", external_id="UCartist")
        )
        self.assertEqual(len(result.results), 200)
        self.assertEqual(result.results[0].source.external_id, "MPREb_0")
        self.assertEqual(result.results[-1].source.external_id, "MPREb_199")
        client.get_artist_albums.assert_not_called()

    def test_fetch_artist_rejects_wrong_identity_before_calling_provider(self) -> None:
        client = Mock()
        with self.assertRaises(SourceNotFoundError):
            YouTubeMusicCatalogue(client).fetch_artist(
                SourceIdentity(provider_id="youtube_music", external_id="not-UC")
            )
        client.get_artist.assert_not_called()

    def test_fetch_album_rejects_a_non_album_id_before_calling_the_provider(
        self,
    ) -> None:
        client = Mock()
        source = SourceIdentity(
            provider_id="youtube_music", external_id="30ce80xVHL4"
        )

        with self.assertRaises(SourceNotFoundError):
            YouTubeMusicCatalogue(client).fetch_album(source)

        client.get_album.assert_not_called()

    def test_fetch_album_rejects_album_ids_with_control_characters(self) -> None:
        for external_id in [
            "MPREb_ok\ninjected_log_line=yes",
            "MPREb_ok\r\nnext",
            "MPREb_ok\tnext",
            "MPREb_ok next",
            "MPRE\u274c",
            "MPRE" + "a" * 125,
        ]:
            with self.subTest(external_id=external_id):
                client = Mock()
                source = SourceIdentity(
                    provider_id="youtube_music", external_id=external_id
                )

                with self.assertRaises(SourceNotFoundError):
                    YouTubeMusicCatalogue(client).fetch_album(source)

                client.get_album.assert_not_called()

    def test_fetch_album_translates_a_rejected_browse_id(self) -> None:
        client = Mock()
        client.get_album.side_effect = YTMusicUserError(
            "Invalid album browseId provided, must start with MPRE."
        )
        source = SourceIdentity(
            provider_id="youtube_music", external_id="MPREb_missing"
        )

        with self.assertRaises(SourceNotFoundError):
            YouTubeMusicCatalogue(client).fetch_album(source)

    def test_fetch_album_keeps_provider_failures_unavailable(self) -> None:
        for failure in [
            YTMusicServerError("private provider details"),
            RequestException("private transport details"),
        ]:
            with self.subTest(failure=type(failure).__name__):
                client = Mock()
                client.get_album.side_effect = failure
                source = SourceIdentity(
                    provider_id="youtube_music", external_id="MPREb_album"
                )

                with self.assertRaises(ProviderUnavailableError):
                    YouTubeMusicCatalogue(client).fetch_album(source)

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
                "alternate_sources": [],
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

    def test_fetch_album_pairs_music_video_tracks_with_their_audio_ids(self) -> None:
        client = Mock()
        client.get_album.return_value = load_catalogue_fixture(
            "ytm_album_ego_death.json"
        )
        client.get_playlist.return_value = load_catalogue_fixture(
            "ytm_playlist_ego_death.json"
        )

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        client.get_playlist.assert_called_once_with("OLAK5uy_ego_death", limit=None)
        self.assertEqual(
            [candidate.source.external_id for candidate in response.results],
            ["ATVglum0001", "ATVkillMe02", "ATVwhim0003"],
        )
        self.assertEqual(
            [
                [alternate.external_id for alternate in candidate.alternate_sources]
                for candidate in response.results
            ],
            [["OMVglum0001"], ["OMVkillMe02"], ["OMVwhim0003"]],
        )
        self.assertEqual(
            [candidate.duration_ms for candidate in response.results],
            [192_000, 168_000, 216_000],
        )
        self.assertEqual(response.results[0].title, "Glum")

    def test_fetch_album_pairing_falls_back_to_unique_titles_on_a_length_mismatch(
        self,
    ) -> None:
        album = load_catalogue_fixture("ytm_album_ego_death.json")
        playlist = load_catalogue_fixture("ytm_playlist_ego_death.json")
        playlist["tracks"] = [
            track for track in playlist["tracks"] if track["title"] != "Whim"
        ]
        client = Mock()
        client.get_album.return_value = album
        client.get_playlist.return_value = playlist

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        self.assertEqual(
            [candidate.source.external_id for candidate in response.results],
            ["ATVglum0001", "ATVkillMe02", "OMVwhim0003"],
        )
        self.assertEqual(
            [len(candidate.alternate_sources) for candidate in response.results],
            [1, 1, 0],
        )
        self.assertEqual(response.results[2].duration_ms, 239_000)

    def test_fetch_album_leaves_a_duplicate_title_unpaired(self) -> None:
        album = load_catalogue_fixture("ytm_album_ego_death.json")
        album["tracks"][1]["title"] = "Glum"
        client = Mock()
        client.get_album.return_value = album
        client.get_playlist.return_value = load_catalogue_fixture(
            "ytm_playlist_ego_death.json"
        )

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        self.assertEqual(
            [candidate.source.external_id for candidate in response.results],
            ["OMVglum0001", "OMVkillMe02", "ATVwhim0003"],
        )
        self.assertEqual(
            [len(candidate.alternate_sources) for candidate in response.results],
            [0, 0, 1],
        )

    def test_fetch_album_keeps_an_atv_track_as_its_own_source(self) -> None:
        album = load_catalogue_fixture("ytm_album_ego_death.json")
        album["tracks"][0]["videoType"] = "MUSIC_VIDEO_TYPE_ATV"
        client = Mock()
        client.get_album.return_value = album
        client.get_playlist.return_value = load_catalogue_fixture(
            "ytm_playlist_ego_death.json"
        )

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        self.assertEqual(response.results[0].source.external_id, "OMVglum0001")
        self.assertEqual(response.results[0].alternate_sources, [])
        self.assertEqual(response.results[0].duration_ms, 200_000)

    def test_fetch_album_logs_one_warning_when_the_playlist_fetch_fails(self) -> None:
        client = Mock()
        client.get_album.return_value = load_catalogue_fixture(
            "ytm_album_ego_death.json"
        )
        client.get_playlist.side_effect = YTMusicServerError("private details")

        with patch("app.youtube.catalogue.logger.warning") as warning:
            response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        warning.assert_called_once()
        self.assertEqual(
            warning.call_args.args,
            (
                "album audio playlist unavailable album_id=%s error_type=%s",
                "MPREb_ego",
                "YTMusicServerError",
            ),
        )
        self.assertEqual(
            [candidate.source.external_id for candidate in response.results],
            ["OMVglum0001", "OMVkillMe02", "OMVwhim0003"],
        )
        self.assertTrue(
            all(not candidate.alternate_sources for candidate in response.results)
        )

    def test_fetch_album_without_an_audio_playlist_keeps_album_tracks(self) -> None:
        album = load_catalogue_fixture("ytm_album_ego_death.json")
        del album["audioPlaylistId"]
        client = Mock()
        client.get_album.return_value = album

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        client.get_playlist.assert_not_called()
        self.assertEqual(
            [candidate.source.external_id for candidate in response.results],
            ["OMVglum0001", "OMVkillMe02", "OMVwhim0003"],
        )
        self.assertEqual(response.results[1].duration_ms, 187_000)

    def test_fetch_album_same_audio_id_keeps_todays_output(self) -> None:
        album = load_catalogue_fixture("ytm_album_ego_death.json")
        playlist = load_catalogue_fixture("ytm_playlist_ego_death.json")
        playlist["tracks"][0]["videoId"] = "OMVglum0001"
        client = Mock()
        client.get_album.return_value = album
        client.get_playlist.return_value = playlist

        response = YouTubeMusicCatalogue(client).fetch_album(album_source())

        self.assertEqual(response.results[0].source.external_id, "OMVglum0001")
        self.assertEqual(response.results[0].alternate_sources, [])
        self.assertEqual(response.results[0].duration_ms, 200_000)
