import json
import unittest
from copy import deepcopy
from datetime import timedelta
from pathlib import Path
from unittest.mock import Mock

from requests.exceptions import RequestException
from ytmusicapi.exceptions import YTMusicServerError, YTMusicUserError

from app.generated.resolver_v1 import (
    CandidateSource,
    ErrorCode,
    MatchBasis,
    SourceIdentity,
)
from app.metadata.document import DocumentError
from app.youtube.artist_metadata import YouTubeMusicArtistMetadata

FIXTURES = Path(__file__).parent / "fixtures" / "metadata"
CHANNEL_ID = "UCIaFw5VBEK8qaW6nRpx_qnw"
LARGEST_THUMBNAIL = (
    "https://lh3.googleusercontent.com/"
    "IOKuXtp8PCQ_Fc-vaRKm3sKIXBxFV51gZheLTH5br-YGnWHFQf_Jywcuk7wbprYRoEbQyS_XZY6-nMJX"
    "=w2048-h853-p-l90-rj"
)
ALL_FIELDS = [
    "biography.content",
    "image.wide",
    "image.portrait",
    "similar",
    "top_tracks",
    "page.url",
]


def load_fixture(name: str):
    with (FIXTURES / name).open(encoding="utf-8") as handle:
        return json.load(handle)


def candidate_fields(document) -> list[str]:
    return [candidate.field for candidate in document.candidates]


def candidate_for(document, field: str):
    for candidate in document.candidates:
        if candidate.field == field:
            return candidate
    raise AssertionError(f"missing candidate {field}")


def error_for(document, field: str):
    for error in document.errors:
        if error.field == field:
            return error
    raise AssertionError(f"missing error {field}")


class YouTubeMusicArtistMetadataTest(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SourceIdentity(
            provider_id="youtube_music", external_id=CHANNEL_ID
        )

    def metadata_for(self, document):
        client = Mock()
        client.get_artist.return_value = document
        return client, YouTubeMusicArtistMetadata(client)

    def test_one_document_yields_every_available_field_from_a_single_fetch(
        self,
    ) -> None:
        document = load_fixture("ytm_artist_document.json")
        client, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(self.source, "en", ALL_FIELDS)

        client.get_artist.assert_called_once_with(CHANNEL_ID)

        biography = candidate_for(result, "biography.content")
        self.assertEqual(biography.text, document["description"])
        self.assertEqual(biography.licence.id, "youtube_music")
        self.assertFalse(biography.licence.redistribution_allowed)
        self.assertTrue(biography.licence.cache_allowed)
        self.assertTrue(biography.licence.uncertainty)
        self.assertEqual(biography.attribution.text, "Coldplay")
        self.assertEqual(biography.language, None)
        self.assertEqual(
            biography.source,
            CandidateSource(
                provider_id="youtube_music",
                external_id=CHANNEL_ID,
                match_basis=MatchBasis.identifier,
            ),
        )

        wide = candidate_for(result, "image.wide")
        self.assertEqual(wide.image.role, "wide")
        self.assertEqual(str(wide.image.reference), LARGEST_THUMBNAIL)
        self.assertEqual(wide.image.width, 2048)
        self.assertEqual(wide.image.height, 853)

        similar = candidate_for(result, "similar")
        self.assertEqual(similar.licence.id, "youtube_music")
        self.assertEqual(similar.attribution.text, "Coldplay")
        self.assertEqual(len(similar.candidates), 10)
        first_similar = similar.candidates[0]
        self.assertEqual(first_similar.name, "M83")
        self.assertEqual(
            first_similar.source,
            CandidateSource(
                provider_id="youtube_music",
                external_id="UC0UUzwcmD3s1S7NYAxHfs1w",
                match_basis=MatchBasis.identifier,
            ),
        )
        self.assertEqual(first_similar.rank, 0)
        self.assertIsNone(first_similar.score)

        top_tracks = candidate_for(result, "top_tracks")
        self.assertEqual(len(top_tracks.candidates), 5)
        first_track = top_tracks.candidates[0]
        self.assertEqual(first_track.name, "Yellow")
        self.assertEqual(
            first_track.source,
            CandidateSource(
                provider_id="youtube_music",
                external_id="9qnqYL0eNNI",
                match_basis=MatchBasis.identifier,
            ),
        )
        self.assertEqual(first_track.artists, ["Coldplay"])
        self.assertEqual(first_track.release_name, "Parachutes")
        self.assertEqual(first_track.duration_ms, 267_000)
        self.assertEqual(first_track.isrc, "GBAYE0000001")
        self.assertIsNone(first_track.recording_id)
        self.assertEqual(first_track.rank, 0)
        self.assertIsNone(first_track.score)
        self.assertEqual(top_tracks.candidates[-1].rank, 4)

        self.assertEqual(
            candidate_fields(result),
            ["biography.content", "image.wide", "similar", "top_tracks"],
        )
        self.assertEqual(result.errors, [])
        self.assertEqual(result.address, self.source)
        self.assertIsNotNone(result.retrieved_at.tzinfo)
        self.assertEqual(result.retrieved_at.utcoffset(), timedelta(0))

    def test_portrait_and_page_url_are_not_invented(self) -> None:
        document = load_fixture("ytm_artist_document.json")
        _, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(
            self.source, "en", ["image.portrait", "page.url"]
        )

        self.assertEqual(candidate_fields(result), [])
        self.assertEqual(result.errors, [])

    def test_the_document_is_fetched_exactly_once_however_many_fields_are_asked_for(
        self,
    ) -> None:
        document = load_fixture("ytm_artist_document.json")
        client, metadata = self.metadata_for(document)

        metadata.fetch_artist_document(self.source, "en", ALL_FIELDS)

        self.assertEqual(client.get_artist.call_count, 1)

    def test_a_requested_field_subset_filters_candidates_after_one_fetch(self) -> None:
        document = load_fixture("ytm_artist_document.json")
        client, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(self.source, "en", ["similar"])

        client.get_artist.assert_called_once_with(CHANNEL_ID)
        self.assertEqual(candidate_fields(result), ["similar"])

    def test_empty_biography_omits_the_candidate_without_an_error(self) -> None:
        document = load_fixture("ytm_artist_document_empty_biography.json")
        client, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.wide"]
        )

        client.get_artist.assert_called_once_with(CHANNEL_ID)
        self.assertEqual(candidate_fields(result), ["image.wide"])
        self.assertEqual(result.errors, [])

    def test_absent_thumbnails_omit_the_image_candidate_without_an_error(self) -> None:
        document = load_fixture("ytm_artist_document_without_thumbnails.json")
        client, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.wide"]
        )

        self.assertEqual(candidate_fields(result), ["biography.content"])
        self.assertEqual(result.errors, [])

    def test_absent_sections_omit_discovery_candidates_without_an_error(self) -> None:
        document = load_fixture("ytm_artist_document_empty_biography.json")
        client, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(
            self.source, "en", ["similar", "top_tracks"]
        )

        client.get_artist.assert_called_once_with(CHANNEL_ID)
        self.assertEqual(candidate_fields(result), [])
        self.assertEqual(result.errors, [])

    def test_every_credit_of_a_multi_artist_top_track_is_collected(self) -> None:
        document = deepcopy(load_fixture("ytm_artist_document.json"))
        document["songs"]["results"][0]["artists"] = [
            {"name": "Coldplay", "id": CHANNEL_ID},
            {"name": "The Chainsmokers", "id": "UCQgUHOPJJrmzCjExg-ISupA"},
        ]
        _, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(self.source, "en", ["top_tracks"])

        self.assertEqual(
            candidate_for(result, "top_tracks").candidates[0].artists,
            ["Coldplay", "The Chainsmokers"],
        )

    def test_biography_over_the_cap_is_omitted_and_reported(self) -> None:
        document = deepcopy(load_fixture("ytm_artist_document.json"))
        document["description"] = "a" * 65537
        _, metadata = self.metadata_for(document)

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.wide"]
        )

        self.assertEqual(candidate_fields(result), ["image.wide"])
        error = error_for(result, "biography.content")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(error.message, "field_too_large")

    def test_a_wrong_payload_type_is_a_resolution_failure(self) -> None:
        document = load_fixture("ytm_artist_document_wrong_payload_type.json")
        client, metadata = self.metadata_for(document)

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)
        client.get_artist.assert_called_once_with(CHANNEL_ID)

    def test_wrong_typed_sections_and_entries_are_resolution_failures(self) -> None:
        for mutate in [
            lambda document: document.update({"related": []}),
            lambda document: document.update({"songs": "not a section"}),
            lambda document: document["songs"]["results"][0].pop("videoId"),
            lambda document: document["songs"]["results"][0].update(
                {"videoId": 5}
            ),
            lambda document: document["related"]["results"][0].pop("browseId"),
            lambda document: document["songs"]["results"][0].update(
                {"artists": "not a list"}
            ),
        ]:
            with self.subTest(mutation=mutate):
                document = deepcopy(load_fixture("ytm_artist_document.json"))
                mutate(document)
                _, metadata = self.metadata_for(document)

                with self.assertRaises(DocumentError) as raised:
                    metadata.fetch_artist_document(self.source, "en", None)

                self.assertEqual(
                    raised.exception.code, ErrorCode.resolution_failed
                )

    def test_unknown_address_is_a_typed_not_found(self) -> None:
        client = Mock()
        client.get_artist.side_effect = KeyError(
            "Unable to find 'contents' using path ['contents'] on {}"
        )
        metadata = YouTubeMusicArtistMetadata(client)

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.source_not_found)
        self.assertFalse(raised.exception.retryable)

    def test_a_user_error_is_a_typed_not_found(self) -> None:
        client = Mock()
        client.get_artist.side_effect = YTMusicUserError("private provider details")
        metadata = YouTubeMusicArtistMetadata(client)

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.source_not_found)

    def test_malformed_documents_are_resolution_failures(self) -> None:
        for failure in [
            TypeError("list indices must be integers or slices, not str"),
            AttributeError("'str' object has no attribute 'get'"),
            IndexError("list index out of range"),
            json.JSONDecodeError("Expecting value", "<html>", 0),
        ]:
            with self.subTest(failure=type(failure).__name__):
                client = Mock()
                client.get_artist.side_effect = failure
                metadata = YouTubeMusicArtistMetadata(client)

                with self.assertRaises(DocumentError) as raised:
                    metadata.fetch_artist_document(self.source, "en", None)

                self.assertEqual(
                    raised.exception.code, ErrorCode.resolution_failed
                )
                self.assertFalse(raised.exception.retryable)

    def test_provider_failures_are_unavailable_and_retryable(self) -> None:
        for failure in [
            YTMusicServerError("Server returned HTTP 429: Too Many Requests."),
            RequestException("private transport details"),
        ]:
            with self.subTest(failure=type(failure).__name__):
                client = Mock()
                client.get_artist.side_effect = failure
                metadata = YouTubeMusicArtistMetadata(client)

                with self.assertRaises(DocumentError) as raised:
                    metadata.fetch_artist_document(self.source, "en", None)

                self.assertEqual(
                    raised.exception.code, ErrorCode.provider_unavailable
                )
                self.assertTrue(raised.exception.retryable)
                self.assertNotIn("private", str(raised.exception))
