import json
import logging
import os
import unittest
from copy import deepcopy
from pathlib import Path
from unittest.mock import Mock, patch

import requests

from app.audiodb.artist_metadata import AudioDbArtistMetadata
from app.audiodb.client import AudioDbClient, configured_key
from app.generated.resolver_v1 import (
    CandidateSource,
    ErrorCode,
    MatchBasis,
    SourceIdentity,
)
from app.metadata.document import DocumentError

FIXTURES = Path(__file__).parent / "fixtures" / "metadata"
MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"
THUMBNAIL = "https://r2.theaudiodb.com/images/media/artist/thumb/uxrqxy1347913147.jpg"
WIDE_THUMBNAIL = (
    "https://r2.theaudiodb.com/images/media/artist/widethumb/sxqspt1516190718.jpg"
)


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


def response(status_code: int, body=None, headers=None, text: str = "") -> Mock:
    result = Mock(spec=requests.Response)
    result.status_code = status_code
    result.headers = headers or {}
    result.text = text
    if body is None:
        result.json.side_effect = ValueError("not json")
    else:
        result.json.return_value = body
    return result


class AudioDbClientTest(unittest.TestCase):
    def test_the_artist_mbid_route_is_requested_once_with_the_configured_key(
        self,
    ) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body={"artists": []})
        client = AudioDbClient("injected-key", session=session, timeout=7.5)

        payload = client.fetch_artist_by_mbid(MBID)

        self.assertEqual(payload, {"artists": []})
        session.get.assert_called_once()
        url = session.get.call_args.args[0]
        self.assertEqual(
            url,
            "https://www.theaudiodb.com/api/v1/json/injected-key/artist-mb.php",
        )
        self.assertEqual(session.get.call_args.kwargs["params"], {"i": MBID})
        self.assertEqual(session.get.call_args.kwargs["timeout"], 7.5)

    def test_a_429_propagates_the_retry_after_delay_as_unavailable(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(429, headers={"Retry-After": "7"})
        client = AudioDbClient("injected-key", session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist_by_mbid(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertEqual(raised.exception.retry_after_seconds, 7.0)

    def test_server_failures_are_unavailable_and_retryable(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(503, text="unavailable")
        client = AudioDbClient("injected-key", session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist_by_mbid(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertIsNone(raised.exception.retry_after_seconds)

    def test_timeout_and_connection_failures_are_unavailable_and_retryable(
        self,
    ) -> None:
        for failure in [
            requests.Timeout("private transport details"),
            requests.ConnectionError("private transport details"),
        ]:
            with self.subTest(failure=type(failure).__name__):
                session = Mock(spec=requests.Session)
                session.get.side_effect = failure
                client = AudioDbClient("injected-key", session=session)

                with self.assertRaises(DocumentError) as raised:
                    client.fetch_artist_by_mbid(MBID)

                self.assertEqual(
                    raised.exception.code, ErrorCode.provider_unavailable
                )
                self.assertTrue(raised.exception.retryable)
                self.assertNotIn("private", str(raised.exception))

    def test_a_non_json_body_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, text="<html>not json</html>")
        client = AudioDbClient("injected-key", session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist_by_mbid(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)

    def test_a_client_error_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(400, text="bad request")
        client = AudioDbClient("injected-key", session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist_by_mbid(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)

    def test_the_key_is_not_logged_or_echoed_in_an_error(self) -> None:
        key = "sentinel-audiodb-key-value"
        mbid = "00000000-0000-0000-0000-0000000000aa"
        failures = [
            requests.ConnectionError("connection refused"),
            requests.Timeout("timed out"),
        ]

        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                session = Mock(spec=requests.Session)
                session.get.side_effect = failure
                client = AudioDbClient(key, session=session)
                records = []
                handler = logging.Handler()
                handler.emit = records.append
                root = logging.getLogger()
                root.addHandler(handler)

                try:
                    with self.assertRaises(DocumentError) as raised:
                        client.fetch_artist_by_mbid(mbid)
                finally:
                    root.removeHandler(handler)

                self.assertNotIn(key, str(raised.exception))
                self.assertNotIn(
                    key, "".join(str(record.getMessage()) for record in records)
                )
                self.assertTrue(raised.exception.__suppress_context__)

    def test_configured_key_reads_the_resolver_environment(self) -> None:
        with patch.dict(
            os.environ, {"WHIO_RESOLVER_AUDIODB_KEY": "  resolved-key  "}
        ):
            self.assertEqual(configured_key(), "resolved-key")

        for value in ["", "   "]:
            with self.subTest(value=value):
                with patch.dict(os.environ, {"WHIO_RESOLVER_AUDIODB_KEY": value}):
                    self.assertIsNone(configured_key())


class AudioDbArtistMetadataTest(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SourceIdentity(provider_id="theaudiodb", external_id=MBID)

    def metadata_for(self, payload):
        client = Mock(spec=AudioDbClient)
        client.fetch_artist_by_mbid.return_value = payload
        return client, AudioDbArtistMetadata(client)

    def test_one_document_yields_biography_portrait_and_wide_from_a_single_fetch(
        self,
    ) -> None:
        artist = load_fixture("audiodb_artist_document.json")["artists"][0]
        client, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.portrait", "image.wide"]
        )

        client.fetch_artist_by_mbid.assert_called_once_with(MBID)

        biography = candidate_for(result, "biography.content")
        self.assertEqual(biography.text, artist["strBiography"])
        self.assertEqual(biography.licence.id, "theaudiodb.terms")
        self.assertFalse(biography.licence.redistribution_allowed)
        self.assertTrue(biography.licence.cache_allowed)
        self.assertTrue(biography.licence.uncertainty)
        self.assertIsNone(biography.language)
        self.assertEqual(biography.attribution.text, "Data provided by TheAudioDB")
        self.assertEqual(
            str(biography.attribution.url),
            "https://www.theaudiodb.com/artist/111239",
        )

        self.assertEqual(
            biography.source,
            CandidateSource(
                provider_id="theaudiodb",
                external_id=MBID,
                match_basis=MatchBasis.established_source,
                url="https://www.theaudiodb.com/artist/111239",
            ),
        )

        portrait = candidate_for(result, "image.portrait")
        self.assertEqual(portrait.image.role, "portrait")
        self.assertEqual(str(portrait.image.reference), THUMBNAIL)
        self.assertIsNone(portrait.image.width)
        self.assertEqual(portrait.licence.id, "theaudiodb.terms")

        wide = candidate_for(result, "image.wide")
        self.assertEqual(wide.image.role, "wide")
        self.assertEqual(str(wide.image.reference), WIDE_THUMBNAIL)

        self.assertEqual(
            candidate_fields(result),
            ["biography.content", "image.portrait", "image.wide"],
        )
        self.assertEqual(result.errors, [])
        self.assertEqual(result.address, self.source)

    def test_the_document_is_fetched_exactly_once_however_many_fields_are_asked_for(
        self,
    ) -> None:
        artist = load_fixture("audiodb_artist_document.json")["artists"][0]
        client, metadata = self.metadata_for({"artists": [artist]})

        metadata.fetch_artist_document(
            self.source,
            "en",
            ["biography.content", "image.portrait", "image.wide", "identity.name"],
        )

        self.assertEqual(client.fetch_artist_by_mbid.call_count, 1)

    def test_the_requested_language_selects_the_biography_member(self) -> None:
        artist = load_fixture("audiodb_artist_document_language_member.json")[
            "artists"
        ][0]

        for language, text, declared in [
            ("en", artist["strBiographyEN"], "en"),
            ("de", artist["strBiographyDE"], "de"),
            (None, artist["strBiographyEN"], "en"),
        ]:
            with self.subTest(language=language):
                _, metadata = self.metadata_for({"artists": [artist]})

                result = metadata.fetch_artist_document(
                    self.source,
                    language,
                    ["biography.content", "biography.language"],
                )

                self.assertEqual(
                    candidate_for(result, "biography.content").text, text
                )
                self.assertEqual(
                    candidate_for(result, "biography.language").language, declared
                )

    def test_empty_biography_omits_the_candidate_without_an_error(self) -> None:
        artist = load_fixture("audiodb_artist_document_empty_biography.json")[
            "artists"
        ][0]
        client, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.portrait"]
        )

        self.assertEqual(candidate_fields(result), ["image.portrait"])
        self.assertEqual(result.errors, [])

    def test_biography_over_the_cap_is_omitted_and_reported(self) -> None:
        artist = deepcopy(
            load_fixture("audiodb_artist_document_empty_biography.json")["artists"][0]
        )
        artist["strBiographyEN"] = "a" * 65537
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.portrait"]
        )

        self.assertNotIn("biography.content", candidate_fields(result))
        error = error_for(result, "biography.content")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(error.message, "field_too_large")

    def test_wide_falls_back_to_the_fanart_member(self) -> None:
        artist = load_fixture("audiodb_artist_document_wide_only.json")["artists"][0]
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["image.portrait", "image.wide"]
        )

        self.assertEqual(candidate_fields(result), ["image.wide"])
        self.assertEqual(
            str(candidate_for(result, "image.wide").image.reference),
            artist["strArtistFanart"],
        )

    def test_a_wrong_typed_content_member_is_a_field_error_that_leaves_the_other_field(
        self,
    ) -> None:
        artist = load_fixture("audiodb_artist_document_wrong_image_member_type.json")[
            "artists"
        ][0]
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.portrait"]
        )

        self.assertEqual(candidate_fields(result), ["biography.content"])
        error = error_for(result, "image.portrait")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)

    def test_creative_commons_artwork_changes_only_the_artwork_licence(self) -> None:
        artist = load_fixture("audiodb_artist_document_creative_commons.json")[
            "artists"
        ][0]
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["biography.content", "image.portrait"]
        )

        self.assertEqual(
            candidate_for(result, "biography.content").licence.id,
            "theaudiodb.terms",
        )
        artwork_licence = candidate_for(result, "image.portrait").licence
        self.assertEqual(artwork_licence.id, "theaudiodb.creativecommons")
        self.assertTrue(artwork_licence.redistribution_allowed)
        self.assertFalse(artwork_licence.uncertainty)

    def test_a_creative_commons_string_signal_is_honoured(self) -> None:
        artist = deepcopy(
            load_fixture("audiodb_artist_document_creative_commons.json")["artists"][0]
        )
        artist["strCreativeCommons"] = "Yes"
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(
            self.source, "en", ["image.portrait"]
        )

        self.assertEqual(
            candidate_for(result, "image.portrait").licence.id,
            "theaudiodb.creativecommons",
        )

    def test_a_document_without_the_artist_page_id_omits_the_source_url(self) -> None:
        artist = deepcopy(
            load_fixture("audiodb_artist_document.json")["artists"][0]
        )
        artist.pop("idArtist")
        _, metadata = self.metadata_for({"artists": [artist]})

        result = metadata.fetch_artist_document(self.source, "en", ["image.portrait"])

        self.assertIsNone(candidate_for(result, "image.portrait").source.url)

    def test_a_document_without_the_mbid_echo_cannot_name_its_producing_source(
        self,
    ) -> None:
        artist = deepcopy(
            load_fixture("audiodb_artist_document_empty_biography.json")["artists"][0]
        )
        artist.pop("strMusicBrainzID")
        _, metadata = self.metadata_for({"artists": [artist]})

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)

    def test_an_unknown_mbid_is_a_typed_not_found(self) -> None:
        payload = load_fixture("audiodb_artist_document_missing.json")
        client, metadata = self.metadata_for(payload)

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.source_not_found)
        self.assertFalse(raised.exception.retryable)
        client.fetch_artist_by_mbid.assert_called_once_with(MBID)

    def test_structural_malformations_are_resolution_failures(self) -> None:
        payloads = [
            load_fixture("audiodb_artist_document_wrong_payload_type.json"),
            load_fixture("audiodb_artist_document_wrong_collection_member_type.json"),
            {"artists": ["not an artist record"]},
            {"artists": [5]},
        ]

        for payload in payloads:
            with self.subTest(payload=payload):
                _, metadata = self.metadata_for(payload)

                with self.assertRaises(DocumentError) as raised:
                    metadata.fetch_artist_document(self.source, "en", None)

                self.assertEqual(
                    raised.exception.code, ErrorCode.resolution_failed
                )

    def test_an_absent_collection_member_is_a_resolution_failure(self) -> None:
        _, metadata = self.metadata_for({})

        with self.assertRaises(DocumentError) as raised:
            metadata.fetch_artist_document(self.source, "en", None)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
