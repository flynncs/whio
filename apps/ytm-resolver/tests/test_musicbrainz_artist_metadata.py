import json
import logging
import os
import time
import unittest
from copy import deepcopy
from pathlib import Path
from unittest.mock import Mock, patch

import requests

from app.generated.resolver_v1 import (
    ErrorCode,
    MatchBasis,
    SourceIdentity,
)
from app.metadata.document import DocumentError
from app.musicbrainz.artist_metadata import MusicBrainzArtistMetadata
from app.musicbrainz.client import (
    MUSICBRAINZ_PROVIDER,
    MusicBrainzClient,
    configured_user_agent,
    configured_wikimedia_user_agent,
)

FIXTURES = Path(__file__).parent / "fixtures" / "metadata"
MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"
WIKIDATA_ENTITY = "Q11647"
USER_AGENT = "whio-resolver/1.0 ( https://example.test/contact )"
WIKIMEDIA_USER_AGENT = "whio-resolver-wikimedia/1.0 ( https://example.test/contact )"
FILE_A = "ColdplayWembley.jpg"
FILE_B = "ColdplayMadrid.jpg"
FILE_URL_A = "https://upload.wikimedia.org/wikipedia/commons/a/a1/ColdplayWembley.jpg"
FILE_URL_B = "https://upload.wikimedia.org/wikipedia/commons/b/b2/ColdplayMadrid.jpg"
FILE_PAGE_A = "https://commons.wikimedia.org/wiki/File:ColdplayWembley.jpg"
FILE_PAGE_B = "https://commons.wikimedia.org/wiki/File:ColdplayMadrid.jpg"
EN_PAGE = "https://en.wikipedia.org/wiki/Coldplay"
DE_PAGE = "https://de.wikipedia.org/wiki/Coldplay"
SUMMARY_TEXT = "Coldplay are a British rock band formed in London in 1997."
EXTRACT_TEXT = (
    "Coldplay are a British rock band formed in London in 1997. They consist of "
    "vocalist and pianist Chris Martin, guitarist Jonny Buckland, bassist Guy "
    "Berryman and drummer Will Champion."
)
DE_SUMMARY_TEXT = (
    "Coldplay ist eine britische Rockband, die 1997 in London gegruendet wurde."
)

ARTIST_FIXTURE = "controlled_musicbrainz_artist_document.json"
WIKIDATA_FIXTURE = "controlled_wikidata_entity.json"
COMMONS_FIXTURE = "controlled_commons_imageinfo.json"
SUMMARY_FIXTURE = "controlled_wikipedia_summary.json"
EXTRACT_FIXTURE = "controlled_wikipedia_extract.json"

ALL_FIELDS = [
    "identity.mbid",
    "page.url",
    "tag",
    "biography.summary",
    "biography.content",
    "biography.language",
    "image.portrait",
]

CREDENTIAL_MARKERS = (
    "token=",
    "sig=",
    "signature=",
    "x-amz-",
    "bearer ",
    "api_key",
    "api-key",
    "authorization:",
    "password",
    "secret",
    "sentinel",
)


def load_fixture(name: str):
    with (FIXTURES / name).open(encoding="utf-8") as handle:
        return json.load(handle)


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


def candidate_fields(document) -> list[str]:
    return [candidate.field for candidate in document.candidates]


def candidates_for(document, field: str) -> list:
    return [
        candidate for candidate in document.candidates if candidate.field == field
    ]


def candidate_for(document, field: str):
    candidates = candidates_for(document, field)
    if not candidates:
        raise AssertionError(f"missing candidate {field}")
    return candidates[0]


def errors_for(document, field: str) -> list:
    return [error for error in document.errors if error.field == field]


def error_for(document, field: str):
    errors = errors_for(document, field)
    if not errors:
        raise AssertionError(f"missing error {field}")
    return errors[0]


def licence_block(candidate) -> dict:
    licence = candidate.licence
    return {
        "id": licence.id,
        "name": licence.name,
        "url": str(licence.url),
        "attribution_required": licence.attribution_required,
        "redistribution_allowed": licence.redistribution_allowed,
        "cache_allowed": licence.cache_allowed,
        "share_alike": licence.share_alike,
        "non_commercial": licence.non_commercial,
        "uncertainty": licence.uncertainty,
    }


class MusicBrainzClientTest(unittest.TestCase):
    def test_the_artist_document_is_requested_once_with_the_user_agent(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(
            200, body=load_fixture(ARTIST_FIXTURE)
        )
        client = MusicBrainzClient(
            USER_AGENT, session=session, musicbrainz_timeout=7.5
        )

        payload = client.fetch_artist(MBID)

        self.assertEqual(payload["id"], MBID)
        session.get.assert_called_once()
        self.assertEqual(
            session.get.call_args.args[0],
            f"https://musicbrainz.org/ws/2/artist/{MBID}",
        )
        self.assertEqual(
            session.get.call_args.kwargs["params"],
            {"inc": "url-rels+tags+genres", "fmt": "json"},
        )
        self.assertEqual(session.get.call_args.kwargs["timeout"], 7.5)
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": USER_AGENT},
        )

    def test_the_wikidata_entity_call_carries_the_wikimedia_user_agent(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(WIKIDATA_FIXTURE))
        client = MusicBrainzClient(
            USER_AGENT,
            WIKIMEDIA_USER_AGENT,
            session=session,
            wikimedia_timeout=9.0,
        )

        payload = client.fetch_wikidata_entity(WIKIDATA_ENTITY)

        self.assertIn(WIKIDATA_ENTITY, payload["entities"])
        self.assertEqual(
            session.get.call_args.args[0],
            "https://www.wikidata.org/w/api.php",
        )
        self.assertEqual(
            session.get.call_args.kwargs["params"],
            {
                "action": "wbgetentities",
                "ids": WIKIDATA_ENTITY,
                "props": "claims|sitelinks",
                "format": "json",
                "maxlag": "5",
            },
        )
        self.assertEqual(session.get.call_args.kwargs["timeout"], 9.0)
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": WIKIMEDIA_USER_AGENT},
        )

    def test_the_commons_imageinfo_call_reads_extmetadata_and_url(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(COMMONS_FIXTURE))
        client = MusicBrainzClient(
            USER_AGENT, WIKIMEDIA_USER_AGENT, session=session
        )

        client.fetch_commons_imageinfo(FILE_A)

        self.assertEqual(
            session.get.call_args.args[0],
            "https://commons.wikimedia.org/w/api.php",
        )
        self.assertEqual(
            session.get.call_args.kwargs["params"],
            {
                "action": "query",
                "prop": "imageinfo",
                "iiprop": "extmetadata|url",
                "titles": f"File:{FILE_A}",
                "format": "json",
                "maxlag": "5",
            },
        )
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": WIKIMEDIA_USER_AGENT},
        )

    def test_the_wikipedia_summary_call_uses_the_page_title_path(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(SUMMARY_FIXTURE))
        client = MusicBrainzClient(USER_AGENT, session=session)

        client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")

        self.assertEqual(
            session.get.call_args.args[0],
            "https://en.wikipedia.org/api/rest_v1/page/summary/Coldplay",
        )
        self.assertEqual(session.get.call_args.kwargs["params"], {})
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": USER_AGENT},
        )

    def test_the_wikipedia_extract_call_uses_the_action_api(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(EXTRACT_FIXTURE))
        client = MusicBrainzClient(
            USER_AGENT, WIKIMEDIA_USER_AGENT, session=session
        )

        client.fetch_wikipedia_extract("de.wikipedia.org", "Coldplay")

        self.assertEqual(
            session.get.call_args.args[0],
            "https://de.wikipedia.org/w/api.php",
        )
        self.assertEqual(
            session.get.call_args.kwargs["params"],
            {
                "action": "query",
                "prop": "extracts",
                "explaintext": "1",
                "titles": "Coldplay",
                "format": "json",
                "maxlag": "5",
            },
        )
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": WIKIMEDIA_USER_AGENT},
        )

    def test_the_wikimedia_user_agent_falls_back_to_the_musicbrainz_one(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(SUMMARY_FIXTURE))
        client = MusicBrainzClient(USER_AGENT, session=session)

        client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")

        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {"User-Agent": USER_AGENT},
        )

    def test_an_unknown_mbid_is_a_typed_not_found(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(
            404, body={"error": "Not Found"}, text="Not Found"
        )
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.source_not_found)
        self.assertFalse(raised.exception.retryable)

    def test_a_musicbrainz_429_is_unavailable_without_a_documented_delay(
        self,
    ) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(429, headers={})
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertIsNone(raised.exception.retry_after_seconds)

    def test_a_wikimedia_429_propagates_the_retry_after_delay(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(429, headers={"Retry-After": "9"})
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertEqual(raised.exception.retry_after_seconds, 9.0)

    def test_a_wikimedia_503_propagates_the_retry_after_delay(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(
            503, headers={"Retry-After": "3"}, text="unavailable"
        )
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_wikipedia_extract("en.wikipedia.org", "Coldplay")

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertEqual(raised.exception.retry_after_seconds, 3.0)

    def test_server_failures_are_unavailable_and_retryable(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(503, text="unavailable")
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist(MBID)

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
                client = MusicBrainzClient(USER_AGENT, session=session)

                with self.assertRaises(DocumentError) as raised:
                    client.fetch_artist(MBID)

                self.assertEqual(
                    raised.exception.code, ErrorCode.provider_unavailable
                )
                self.assertTrue(raised.exception.retryable)
                self.assertNotIn("private", str(raised.exception))

    def test_a_non_json_body_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, text="<html>not json</html>")
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)

    def test_a_client_error_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(400, text="bad request")
        client = MusicBrainzClient(USER_AGENT, session=session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_artist(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)

    def test_the_musicbrainz_host_is_paced_between_requests(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(ARTIST_FIXTURE))
        client = MusicBrainzClient(
            USER_AGENT, session=session, musicbrainz_minimum_interval=0.05
        )

        started = time.monotonic()
        client.fetch_artist(MBID)
        client.fetch_artist(MBID)
        elapsed = time.monotonic() - started

        self.assertEqual(session.get.call_count, 2)
        self.assertGreaterEqual(elapsed, 0.05)

    def test_a_wikimedia_host_is_not_paced_between_requests(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=load_fixture(SUMMARY_FIXTURE))
        client = MusicBrainzClient(USER_AGENT, session=session)

        with patch("app.musicbrainz.client.time.sleep") as sleep:
            client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
            client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")

        sleep.assert_not_called()

    def test_the_user_agent_is_not_logged_or_echoed_in_an_error(self) -> None:
        user_agent = "whio-resolver/1.0 ( sentinel-user-agent-value )"
        failures = [
            requests.ConnectionError("connection refused"),
            requests.Timeout("timed out"),
        ]

        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                session = Mock(spec=requests.Session)
                session.get.side_effect = failure
                client = MusicBrainzClient(user_agent, session=session)
                records = []
                handler = logging.Handler()
                handler.emit = records.append
                root = logging.getLogger()
                root.addHandler(handler)

                try:
                    with self.assertRaises(DocumentError) as raised:
                        client.fetch_artist(MBID)
                finally:
                    root.removeHandler(handler)

                self.assertNotIn(user_agent, str(raised.exception))
                self.assertNotIn(
                    user_agent,
                    "".join(str(record.getMessage()) for record in records),
                )
                self.assertTrue(raised.exception.__suppress_context__)

    def test_configured_user_agents_read_the_resolver_environment(self) -> None:
        with patch.dict(
            os.environ,
            {
                "WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT": "  whio/1.0 ( a )  ",
                "WHIO_RESOLVER_WIKIMEDIA_USER_AGENT": "  whio/1.0 ( b )  ",
            },
        ):
            self.assertEqual(configured_user_agent(), "whio/1.0 ( a )")
            self.assertEqual(configured_wikimedia_user_agent(), "whio/1.0 ( b )")

        for value in ["", "   "]:
            with self.subTest(value=value):
                with patch.dict(
                    os.environ,
                    {
                        "WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT": value,
                        "WHIO_RESOLVER_WIKIMEDIA_USER_AGENT": value,
                    },
                ):
                    self.assertIsNone(configured_user_agent())
                    self.assertIsNone(configured_wikimedia_user_agent())


class MusicBrainzArtistMetadataTest(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SourceIdentity(
            provider_id=MUSICBRAINZ_PROVIDER, external_id=MBID
        )

    def metadata_for(
        self,
        artist=None,
        wikidata=None,
        commons=None,
        commons_many=None,
        summary=None,
        extract=None,
    ):
        client = Mock(spec=MusicBrainzClient)
        client.fetch_artist.return_value = (
            load_fixture(ARTIST_FIXTURE) if artist is None else artist
        )
        client.fetch_wikidata_entity.return_value = (
            load_fixture(WIKIDATA_FIXTURE) if wikidata is None else wikidata
        )
        if commons_many is not None:
            client.fetch_commons_imageinfo.side_effect = commons_many
        else:
            client.fetch_commons_imageinfo.return_value = (
                load_fixture(COMMONS_FIXTURE) if commons is None else commons
            )
        client.fetch_wikipedia_summary.return_value = (
            load_fixture(SUMMARY_FIXTURE) if summary is None else summary
        )
        client.fetch_wikipedia_extract.return_value = (
            load_fixture(EXTRACT_FIXTURE) if extract is None else extract
        )
        return client, MusicBrainzArtistMetadata(client)

    def document(
        self,
        metadata,
        fields=None,
        language=None,
    ):
        return metadata.fetch_artist_document(self.source, language, fields)

    def test_one_mbid_document_returns_identity_page_url_tags_and_the_chain(
        self,
    ) -> None:
        client, metadata = self.metadata_for()

        document = self.document(metadata, fields=ALL_FIELDS, language="en")

        client.fetch_artist.assert_called_once_with(MBID)
        client.fetch_wikidata_entity.assert_called_once_with(WIKIDATA_ENTITY)
        client.fetch_wikipedia_summary.assert_called_once_with(
            "en.wikipedia.org", "Coldplay"
        )
        client.fetch_wikipedia_extract.assert_called_once_with(
            "en.wikipedia.org", "Coldplay"
        )
        client.fetch_commons_imageinfo.assert_called_once_with(FILE_A)

        self.assertEqual(document.address, self.source)
        self.assertEqual(document.errors, [])
        self.assertEqual(
            candidate_fields(document),
            [
                "identity.mbid",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "tag",
                "image.portrait",
                "biography.summary",
                "biography.content",
                "biography.language",
            ],
        )

        identity = candidate_for(document, "identity.mbid")
        self.assertEqual(identity.text, MBID)
        self.assertEqual(identity.source.provider_id, "musicbrainz")
        self.assertEqual(identity.source.external_id, MBID)
        self.assertEqual(identity.source.match_basis, MatchBasis.identifier)
        self.assertEqual(
            str(identity.source.url),
            f"https://musicbrainz.org/artist/{MBID}",
        )
        self.assertIsNone(identity.attribution)
        self.assertEqual(
            licence_block(identity),
            {
                "id": "musicbrainz.core",
                "name": "CC0 1.0 Universal (Public Domain Dedication)",
                "url": "https://creativecommons.org/publicdomain/zero/1.0/",
                "attribution_required": False,
                "redistribution_allowed": True,
                "cache_allowed": True,
                "share_alike": False,
                "non_commercial": False,
                "uncertainty": False,
            },
        )

        page_urls = candidates_for(document, "page.url")
        self.assertEqual(
            [str(candidate.source.url) for candidate in page_urls],
            [
                "https://www.coldplay.com/",
                EN_PAGE,
                "https://www.wikidata.org/wiki/Q11647",
                "https://www.discogs.com/artist/39116",
                "https://www.last.fm/music/Coldplay",
            ],
        )
        self.assertEqual([candidate.rank for candidate in page_urls], [0, 1, 2, 3, 4])
        for candidate in page_urls:
            self.assertEqual(candidate.source.provider_id, "musicbrainz")
            self.assertEqual(
                candidate.source.match_basis, MatchBasis.established_source
            )
            self.assertEqual(candidate.licence.id, "musicbrainz.core")
            self.assertIsNone(candidate.attribution)

        self.assertNotIn(
            "last.fm",
            [candidate.source.provider_id for candidate in document.candidates],
        )
        self.assertEqual(
            str(candidates_for(document, "page.url")[4].source.url),
            "https://www.last.fm/music/Coldplay",
        )

        tags = candidate_for(document, "tag")
        self.assertEqual(
            [item.name for item in tags.candidates],
            ["rock", "alternative rock", "pop rock"],
        )
        self.assertEqual([item.rank for item in tags.candidates], [0, 1, 2])
        self.assertEqual(
            [item.source.provider_id for item in tags.candidates],
            ["musicbrainz", "musicbrainz", "musicbrainz"],
        )
        self.assertEqual(
            licence_block(tags),
            {
                "id": "musicbrainz.supplementary",
                "name": "CC BY-NC-SA 3.0",
                "url": "https://creativecommons.org/licenses/by-nc-sa/3.0/",
                "attribution_required": True,
                "redistribution_allowed": True,
                "cache_allowed": True,
                "share_alike": True,
                "non_commercial": True,
                "uncertainty": False,
            },
        )
        self.assertEqual(tags.attribution.text, "MusicBrainz")
        self.assertEqual(str(tags.attribution.url), "https://musicbrainz.org/")

        portrait = candidate_for(document, "image.portrait")
        self.assertEqual(portrait.source.provider_id, "commons")
        self.assertEqual(portrait.source.external_id, FILE_A)
        self.assertEqual(str(portrait.source.url), FILE_PAGE_A)
        self.assertEqual(portrait.rank, 0)
        self.assertEqual(portrait.image.role, "portrait")
        self.assertEqual(str(portrait.image.reference), FILE_URL_A)
        self.assertEqual(portrait.attribution.text, "Ricardo")
        self.assertEqual(
            licence_block(portrait),
            {
                "id": "commons:CC BY 4.0",
                "name": "CC BY 4.0",
                "url": "https://creativecommons.org/licenses/by/4.0/",
                "attribution_required": True,
                "redistribution_allowed": True,
                "cache_allowed": True,
                "share_alike": False,
                "non_commercial": False,
                "uncertainty": False,
            },
        )
        self.assertEqual(
            portrait.licence.terms_basis,
            "Commons imageinfo extmetadata: LicenseShortName, LicenseUrl, "
            "Artist, Credit, AttributionRequired",
        )

        summary = candidate_for(document, "biography.summary")
        self.assertEqual(summary.text, SUMMARY_TEXT)
        self.assertEqual(summary.source.provider_id, "wikipedia")
        self.assertEqual(summary.source.external_id, "Coldplay")
        self.assertEqual(str(summary.source.url), EN_PAGE)
        self.assertEqual(summary.attribution.text, "Coldplay")
        self.assertEqual(str(summary.attribution.url), EN_PAGE)
        self.assertEqual(
            licence_block(summary),
            {
                "id": "wikipedia.cc-by-sa",
                "name": "CC BY-SA 4.0",
                "url": "https://creativecommons.org/licenses/by-sa/4.0/",
                "attribution_required": True,
                "redistribution_allowed": True,
                "cache_allowed": True,
                "share_alike": True,
                "non_commercial": False,
                "uncertainty": False,
            },
        )

        content = candidate_for(document, "biography.content")
        self.assertEqual(content.text, EXTRACT_TEXT)
        self.assertEqual(licence_block(content), licence_block(summary))
        self.assertEqual(content.attribution.text, "Coldplay")

        language = candidate_for(document, "biography.language")
        self.assertEqual(language.language, "en")
        self.assertEqual(language.source.provider_id, "wikipedia")
        self.assertEqual(licence_block(language), licence_block(summary))
        self.assertEqual(language.attribution.text, "Coldplay")

    def test_the_same_commons_file_from_the_image_rel_and_p18_is_not_merged(
        self,
    ) -> None:
        client, metadata = self.metadata_for()

        document = self.document(metadata, fields=["image.portrait"])

        portraits = candidates_for(document, "image.portrait")
        self.assertEqual(len(portraits), 1)
        self.assertEqual(portraits[0].source.external_id, FILE_A)
        client.fetch_commons_imageinfo.assert_called_once_with(FILE_A)

    def test_a_commons_image_rel_is_the_portrait_without_a_wikidata_call(
        self,
    ) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            )
        )

        document = self.document(metadata, fields=["image.portrait"])

        portrait = candidate_for(document, "image.portrait")
        self.assertEqual(portrait.source.external_id, FILE_A)
        self.assertEqual(str(portrait.image.reference), FILE_URL_A)
        client.fetch_wikidata_entity.assert_not_called()
        client.fetch_commons_imageinfo.assert_called_once_with(FILE_A)
        self.assertEqual(document.errors, [])

    def test_a_non_commons_image_rel_falls_through_to_p18(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_non_commons_image.json"
            )
        )

        document = self.document(metadata, fields=["image.portrait"])

        portrait = candidate_for(document, "image.portrait")
        self.assertEqual(portrait.source.external_id, FILE_A)
        client.fetch_commons_imageinfo.assert_called_once_with(FILE_A)
        emitted = [
            str(candidate.source.url)
            for candidate in document.candidates
            if candidate.source.url is not None
        ] + [
            str(candidate.image.reference)
            for candidate in document.candidates
            if candidate.image is not None
        ]
        self.assertNotIn("img.example", " ".join(emitted))

    def test_a_non_commons_image_rel_without_p18_yields_no_portrait(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_non_commons_image.json"
            ),
            wikidata=load_fixture("controlled_wikidata_entity_no_p18.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        self.assertEqual(candidates_for(document, "image.portrait"), [])
        self.assertEqual(errors_for(document, "image.portrait"), [])
        client.fetch_commons_imageinfo.assert_not_called()

    def test_multiple_p18_claims_are_two_candidates_in_claim_order(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_wikidata_only.json"
            ),
            wikidata=load_fixture("controlled_wikidata_entity_two_p18.json"),
            commons_many=[
                load_fixture(COMMONS_FIXTURE),
                load_fixture("controlled_commons_imageinfo_share_alike.json"),
            ],
        )

        document = self.document(metadata, fields=["image.portrait"])

        portraits = candidates_for(document, "image.portrait")
        self.assertEqual(
            [portrait.source.external_id for portrait in portraits],
            [FILE_A, FILE_B],
        )
        self.assertEqual([portrait.rank for portrait in portraits], [0, 1])
        self.assertEqual(
            [portrait.licence.id for portrait in portraits],
            ["commons:CC BY 4.0", "commons:CC BY-SA 4.0"],
        )
        self.assertEqual(
            [portrait.licence.share_alike for portrait in portraits],
            [False, True],
        )
        self.assertEqual(
            [str(portrait.source.url) for portrait in portraits],
            [FILE_PAGE_A, FILE_PAGE_B],
        )
        self.assertEqual(
            [str(portrait.image.reference) for portrait in portraits],
            [FILE_URL_A, FILE_URL_B],
        )
        self.assertEqual(
            client.fetch_commons_imageinfo.call_args_list[0].args, (FILE_A,)
        )
        self.assertEqual(
            client.fetch_commons_imageinfo.call_args_list[1].args, (FILE_B,)
        )
        self.assertEqual(document.errors, [])

    def test_attribution_required_without_a_credit_stays_uncertain(self) -> None:
        _, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            ),
            commons=load_fixture("controlled_commons_imageinfo_no_credit.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        portrait = candidate_for(document, "image.portrait")
        self.assertIsNone(portrait.attribution)
        self.assertEqual(portrait.licence.id, "commons:CC BY-SA 4.0")
        self.assertTrue(portrait.licence.attribution_required)
        self.assertTrue(portrait.licence.share_alike)
        self.assertTrue(portrait.licence.uncertainty)
        self.assertFalse(portrait.licence.non_commercial)
        self.assertTrue(portrait.licence.redistribution_allowed)
        self.assertEqual(document.errors, [])

    def test_a_cc0_file_needs_no_attribution(self) -> None:
        _, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            ),
            commons=load_fixture("controlled_commons_imageinfo_cc0.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        portrait = candidate_for(document, "image.portrait")
        self.assertEqual(portrait.licence.id, "commons:CC0")
        self.assertFalse(portrait.licence.attribution_required)
        self.assertIsNone(portrait.attribution)
        self.assertFalse(portrait.licence.uncertainty)
        self.assertFalse(portrait.licence.share_alike)

    def test_no_p18_and_no_image_rel_yields_no_portrait_and_no_error(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_wikidata_only.json"
            ),
            wikidata=load_fixture("controlled_wikidata_entity_no_p18.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        self.assertEqual(candidates_for(document, "image.portrait"), [])
        self.assertEqual(errors_for(document, "image.portrait"), [])
        client.fetch_commons_imageinfo.assert_not_called()

    def test_a_missing_wikidata_entity_is_absence_not_an_error(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_wikidata_only.json"
            ),
            wikidata=load_fixture("controlled_wikidata_entity_missing.json"),
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "image.portrait"), [])
        self.assertEqual(candidates_for(document, "biography.summary"), [])
        self.assertEqual(document.errors, [])
        client.fetch_commons_imageinfo.assert_not_called()
        client.fetch_wikipedia_summary.assert_not_called()

    def test_a_wikidata_step_failure_only_fails_the_fields_it_blocked(self) -> None:
        client, metadata = self.metadata_for()
        client.fetch_wikidata_entity.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(
            candidate_fields(document),
            [
                "identity.mbid",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "page.url",
                "tag",
                "image.portrait",
                "biography.summary",
                "biography.content",
                "biography.language",
            ],
        )
        self.assertEqual(len(document.errors), 1)
        error = error_for(document, "image.portrait")
        self.assertEqual(error.code, ErrorCode.provider_unavailable)
        self.assertTrue(error.retryable)
        self.assertIsNone(error.message)
        self.assertEqual(
            candidate_for(document, "image.portrait").source.external_id, FILE_A
        )

    def test_a_wikidata_step_failure_without_a_wikipedia_rel_fails_the_biography(
        self,
    ) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_wikidata_only.json"
            )
        )
        client.fetch_wikidata_entity.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(
            candidate_fields(document),
            ["identity.mbid", "page.url", "page.url"],
        )
        self.assertEqual(
            [error.field for error in document.errors],
            [
                "image.portrait",
                "biography.summary",
                "biography.content",
                "biography.language",
            ],
        )
        for error in document.errors:
            self.assertEqual(error.code, ErrorCode.provider_unavailable)
            self.assertTrue(error.retryable)

    def test_a_wikipedia_summary_failure_is_one_field_error(self) -> None:
        client, metadata = self.metadata_for()
        client.fetch_wikipedia_summary.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True, retry_after_seconds=4
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "biography.summary"), [])
        self.assertEqual(
            candidate_for(document, "biography.content").text, EXTRACT_TEXT
        )
        self.assertEqual(
            candidate_for(document, "biography.language").language, "en"
        )
        self.assertEqual(
            [(error.field, error.code, error.retryable) for error in document.errors],
            [("biography.summary", ErrorCode.provider_unavailable, True)],
        )

    def test_a_commons_step_failure_is_a_portrait_field_error(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            )
        )
        client.fetch_commons_imageinfo.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True
        )

        document = self.document(
            metadata, fields=["identity.mbid", "image.portrait"]
        )

        self.assertEqual(
            candidate_fields(document), ["identity.mbid"]
        )
        error = error_for(document, "image.portrait")
        self.assertEqual(error.code, ErrorCode.provider_unavailable)
        self.assertTrue(error.retryable)

    def test_a_wrong_typed_wikidata_claims_member_is_a_portrait_field_error(
        self,
    ) -> None:
        client, metadata = self.metadata_for(
            wikidata=load_fixture("controlled_wikidata_entity_wrong_claims_type.json")
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        error = error_for(document, "image.portrait")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(
            candidate_for(document, "biography.summary").text, SUMMARY_TEXT
        )
        self.assertEqual(
            candidate_for(document, "image.portrait").source.external_id, FILE_A
        )
        client.fetch_commons_imageinfo.assert_called_once_with(FILE_A)

    def test_a_wrong_typed_extract_member_is_a_content_field_error(self) -> None:
        _, metadata = self.metadata_for(
            extract=load_fixture("controlled_wikipedia_extract_wrong_page_type.json")
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(
            candidate_for(document, "biography.summary").text, SUMMARY_TEXT
        )
        self.assertEqual(candidates_for(document, "biography.content"), [])
        error = error_for(document, "biography.content")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(
            candidate_for(document, "biography.language").language, "en"
        )

    def test_a_disambiguation_page_yields_no_biography_candidate(self) -> None:
        client, metadata = self.metadata_for(
            summary=load_fixture("controlled_wikipedia_summary_disambiguation.json")
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "biography.summary"), [])
        self.assertEqual(candidates_for(document, "biography.content"), [])
        self.assertEqual(candidates_for(document, "biography.language"), [])
        self.assertEqual(document.errors, [])
        client.fetch_wikipedia_extract.assert_not_called()

    def test_an_empty_extract_yields_no_biography_placeholder(self) -> None:
        _, metadata = self.metadata_for(
            summary=load_fixture("controlled_wikipedia_summary_no_extract.json"),
            extract=load_fixture("controlled_wikipedia_extract_empty.json"),
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "biography.summary"), [])
        self.assertEqual(candidates_for(document, "biography.content"), [])
        self.assertEqual(candidates_for(document, "biography.language"), [])
        self.assertEqual(document.errors, [])

    def test_biography_language_comes_from_the_wiki_host(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_german_wiki.json"
            ),
            summary=load_fixture("controlled_wikipedia_summary_german.json"),
            extract=load_fixture("controlled_wikipedia_extract_german.json"),
        )

        document = self.document(metadata, fields=ALL_FIELDS, language="en")

        client.fetch_wikipedia_summary.assert_called_once_with(
            "de.wikipedia.org", "Coldplay"
        )
        language = candidate_for(document, "biography.language")
        self.assertEqual(language.language, "de")
        self.assertEqual(
            str(candidate_for(document, "biography.summary").attribution.url),
            DE_PAGE,
        )
        self.assertEqual(
            candidate_for(document, "biography.summary").text, DE_SUMMARY_TEXT
        )

    def test_a_wikidata_sitelink_supplies_the_wiki_target(self) -> None:
        for language, host, declared, summary_fixture, extract_fixture in [
            (
                "de",
                "de.wikipedia.org",
                "de",
                "controlled_wikipedia_summary_german.json",
                "controlled_wikipedia_extract_german.json",
            ),
            (
                None,
                "en.wikipedia.org",
                "en",
                SUMMARY_FIXTURE,
                EXTRACT_FIXTURE,
            ),
        ]:
            with self.subTest(language=language):
                client, metadata = self.metadata_for(
                    artist=load_fixture(
                        "controlled_musicbrainz_artist_document_wikidata_only.json"
                    ),
                    wikidata=load_fixture(
                        "controlled_wikidata_entity_german_sitelink.json"
                    ),
                    summary=load_fixture(summary_fixture),
                    extract=load_fixture(extract_fixture),
                )

                document = self.document(
                    metadata, fields=["biography.language"], language=language
                )

                client.fetch_wikipedia_summary.assert_called_once_with(
                    host, "Coldplay"
                )
                self.assertEqual(
                    candidate_for(document, "biography.language").language,
                    declared,
                )
                self.assertEqual(document.errors, [])

    def test_an_over_cap_summary_is_omitted_never_clamped(self) -> None:
        summary = deepcopy(load_fixture(SUMMARY_FIXTURE))
        summary["extract"] = "a" * 4097
        _, metadata = self.metadata_for(summary=summary)

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "biography.summary"), [])
        error = error_for(document, "biography.summary")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(error.message, "field_too_large")
        self.assertEqual(
            candidate_for(document, "biography.content").text, EXTRACT_TEXT
        )
        self.assertEqual(
            candidate_for(document, "biography.language").language, "en"
        )

    def test_an_over_cap_extract_is_omitted_never_clamped(self) -> None:
        extract = deepcopy(load_fixture(EXTRACT_FIXTURE))
        extract["query"]["pages"]["1"]["extract"] = "a" * 65537
        _, metadata = self.metadata_for(extract=extract)

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidates_for(document, "biography.content"), [])
        error = error_for(document, "biography.content")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(error.message, "field_too_large")
        self.assertEqual(
            candidate_for(document, "biography.summary").text, SUMMARY_TEXT
        )

    def test_a_summary_at_the_cap_is_kept(self) -> None:
        summary = deepcopy(load_fixture(SUMMARY_FIXTURE))
        summary["extract"] = "a" * 4096
        _, metadata = self.metadata_for(summary=summary)

        document = self.document(metadata, fields=["biography.summary"])

        self.assertEqual(candidate_for(document, "biography.summary").text, "a" * 4096)
        self.assertEqual(document.errors, [])

    def test_tags_are_capped_at_32_entries_and_128_bytes_each(self) -> None:
        artist = deepcopy(load_fixture(ARTIST_FIXTURE))
        artist["tags"] = [{"count": 1, "name": "x" * 129}] + [
            {"count": 1, "name": f"tag{index}"} for index in range(40)
        ]
        artist["genres"] = []
        _, metadata = self.metadata_for(artist=artist)

        document = self.document(metadata, fields=["tag"])

        tags = candidate_for(document, "tag")
        self.assertEqual(len(tags.candidates), 32)
        self.assertEqual(tags.candidates[0].name, "tag0")
        self.assertEqual(tags.candidates[-1].name, "tag31")
        self.assertNotIn("x" * 129, [item.name for item in tags.candidates])
        self.assertEqual(document.errors, [])

    def test_only_the_requested_fields_cost_a_chain_call(self) -> None:
        client, metadata = self.metadata_for()

        document = self.document(metadata, fields=["identity.mbid"])

        self.assertEqual(candidate_fields(document), ["identity.mbid"])
        client.fetch_artist.assert_called_once_with(MBID)
        client.fetch_wikidata_entity.assert_not_called()
        client.fetch_commons_imageinfo.assert_not_called()
        client.fetch_wikipedia_summary.assert_not_called()
        client.fetch_wikipedia_extract.assert_not_called()
        self.assertEqual(document.errors, [])

    def test_a_document_without_relations_or_tags_is_identity_only(self) -> None:
        _, metadata = self.metadata_for(
            artist={"id": MBID, "name": "Coldplay"}
        )

        document = self.document(metadata, fields=ALL_FIELDS)

        self.assertEqual(candidate_fields(document), ["identity.mbid"])
        self.assertEqual(document.errors, [])

    def test_a_missing_commons_file_is_a_portrait_field_error(self) -> None:
        client, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            ),
            commons=load_fixture("controlled_commons_imageinfo_missing.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        self.assertEqual(candidates_for(document, "image.portrait"), [])
        error = error_for(document, "image.portrait")
        self.assertEqual(error.code, ErrorCode.resolution_failed)
        self.assertFalse(error.retryable)
        self.assertEqual(document.errors[0].field, "image.portrait")

    def test_a_commons_response_for_another_file_is_not_attributed(self) -> None:
        _, metadata = self.metadata_for(
            artist=load_fixture(
                "controlled_musicbrainz_artist_document_commons_image_only.json"
            ),
            commons=load_fixture("controlled_commons_imageinfo_other_title.json"),
        )

        document = self.document(metadata, fields=["image.portrait"])

        self.assertEqual(candidates_for(document, "image.portrait"), [])
        error = error_for(document, "image.portrait")
        self.assertEqual(error.code, ErrorCode.resolution_failed)

    def test_a_malformed_musicbrainz_document_is_a_resolution_failure(self) -> None:
        payloads = [
            load_fixture(
                "controlled_musicbrainz_artist_document_wrong_relations_type.json"
            ),
            load_fixture(
                "controlled_musicbrainz_artist_document_wrong_identity.json"
            ),
            load_fixture(
                "controlled_musicbrainz_artist_document_wrong_tag_type.json"
            ),
            ["not", "a", "document"],
            {"name": "Coldplay"},
            {"id": ""},
        ]

        for payload in payloads:
            with self.subTest(payload=payload):
                _, metadata = self.metadata_for(artist=payload)

                with self.assertRaises(DocumentError) as raised:
                    self.document(metadata, fields=ALL_FIELDS)

                self.assertEqual(
                    raised.exception.code, ErrorCode.resolution_failed
                )
                self.assertFalse(raised.exception.retryable)

    def test_the_controlled_fixtures_carry_no_credential_or_signed_url(self) -> None:
        fixtures = sorted(FIXTURES.glob("controlled_*.json"))

        self.assertTrue(fixtures)
        for fixture in fixtures:
            with self.subTest(fixture=fixture.name):
                text = fixture.read_text(encoding="utf-8").lower()
                for marker in CREDENTIAL_MARKERS:
                    self.assertNotIn(marker, text)
