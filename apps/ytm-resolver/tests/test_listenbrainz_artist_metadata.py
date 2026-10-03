import json
import logging
import os
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import requests

from app.generated.resolver_v1 import ErrorCode, MatchBasis, SourceIdentity
from app.listenbrainz.artist_metadata import (
    LISTENBRAINZ_OUTPUT_LICENCE_ID,
    LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE,
    ListenBrainzArtistMetadata,
)
from app.listenbrainz.client import (
    INSTANCE_DISCOVERY_ENTRY_LIMIT,
    LISTENBRAINZ_ALGORITHM_VARIABLE,
    LISTENBRAINZ_DEFAULT_ALGORITHM,
    LISTENBRAINZ_TOKEN_VARIABLE,
    ListenBrainzClient,
    configured_algorithm,
    configured_token,
)
from app.metadata.document import DocumentError

FIXTURES = Path(__file__).parent / "fixtures" / "metadata"
MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"
SENTINEL_TOKEN = "sentinel-listenbrainz-token-value"
USER_AGENT = "whio-resolver/1.0 ( https://example.test/contact )"


def load_fixture(name: str):
    with (FIXTURES / name).open(encoding="utf-8") as handle:
        return json.load(handle)


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


def candidate_fields(document) -> list[str]:
    return [candidate.field for candidate in document.candidates]


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


def stream_response(chunks, status_code: int = 200, headers=None) -> Mock:
    result = Mock(spec=requests.Response)
    result.status_code = status_code
    result.headers = headers or {}
    result.iter_content.return_value = iter(chunks)
    result.json.side_effect = ValueError("not json")
    return result


class ListenBrainzClientTest(unittest.TestCase):
    def client_for(self, session, token=None, algorithm=None) -> ListenBrainzClient:
        return ListenBrainzClient(
            token,
            USER_AGENT,
            algorithm or LISTENBRAINZ_DEFAULT_ALGORITHM,
            session=session,
            timeout=7.5,
            minimum_interval=0.0,
        )

    def test_the_similar_artists_call_targets_the_labs_host_with_the_algorithm(
        self,
    ) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(
            200, body=load_fixture("controlled_listenbrainz_similar_artists.json")
        )
        client = self.client_for(session)

        payload = client.fetch_similar_artists(MBID)

        self.assertEqual(len(payload), 3)
        session.get.assert_called_once()
        self.assertEqual(
            session.get.call_args.args[0],
            "https://labs.api.listenbrainz.org/similar-artists/json",
        )
        self.assertEqual(
            session.get.call_args.kwargs["params"],
            {"artist_mbids": MBID, "algorithm": LISTENBRAINZ_DEFAULT_ALGORITHM},
        )
        self.assertEqual(
            session.get.call_args.kwargs["headers"], {"User-Agent": USER_AGENT}
        )
        self.assertEqual(session.get.call_args.kwargs["timeout"], 7.5)
        session.post.assert_not_called()

    def test_the_top_recordings_call_streams_the_api_host_with_the_token(
        self,
    ) -> None:
        entries = load_fixture("controlled_listenbrainz_top_recordings.json")
        session = Mock(spec=requests.Session)
        session.get.return_value = stream_response([json.dumps(entries)])
        client = self.client_for(session, token=SENTINEL_TOKEN)

        payload = client.fetch_top_recordings(MBID)

        self.assertEqual(payload, entries)
        session.get.assert_called_once()
        self.assertEqual(
            session.get.call_args.args[0],
            "https://api.listenbrainz.org/1/popularity/top-recordings-for-artist/"
            + MBID,
        )
        self.assertNotIn("params", session.get.call_args.kwargs)
        self.assertTrue(session.get.call_args.kwargs["stream"])
        self.assertEqual(
            session.get.call_args.kwargs["headers"],
            {
                "User-Agent": USER_AGENT,
                "Authorization": "Token " + SENTINEL_TOKEN,
            },
        )
        session.get.return_value.close.assert_called_once()

    def test_the_artist_popularity_call_posts_the_single_mbid(self) -> None:
        entry = load_fixture("controlled_listenbrainz_artist_popularity.json")
        session = Mock(spec=requests.Session)
        session.post.return_value = response(200, body=entry)
        client = self.client_for(session, token=SENTINEL_TOKEN)

        payload = client.fetch_artist_popularity(MBID)

        self.assertEqual(payload, entry)
        session.post.assert_called_once()
        self.assertEqual(
            session.post.call_args.args[0],
            "https://api.listenbrainz.org/1/popularity/artist",
        )
        self.assertEqual(
            session.post.call_args.kwargs["json"], {"artist_mbids": [MBID]}
        )
        self.assertEqual(
            session.post.call_args.kwargs["headers"],
            {
                "User-Agent": USER_AGENT,
                "Authorization": "Token " + SENTINEL_TOKEN,
            },
        )
        self.assertEqual(session.post.call_args.kwargs["timeout"], 7.5)
        session.get.assert_not_called()

    def test_the_token_is_not_sent_to_the_labs_host(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=[])
        client = self.client_for(session, token=SENTINEL_TOKEN)

        client.fetch_similar_artists(MBID)

        self.assertNotIn("Authorization", session.get.call_args.kwargs["headers"])

    def test_a_streamed_list_stops_at_the_instance_limit(self) -> None:
        entries = load_fixture(
            "controlled_listenbrainz_top_recordings_over_limit.json"
        )
        session = Mock(spec=requests.Session)
        session.get.return_value = stream_response(
            [json.dumps(entries), "TRAILING-BODY-NOT-JSON"]
        )
        client = self.client_for(session)

        payload = client.fetch_top_recordings(MBID)

        self.assertEqual(len(entries), 51)
        self.assertEqual(len(payload), INSTANCE_DISCOVERY_ENTRY_LIMIT)

    def test_a_chunk_split_across_an_entry_is_still_parsed(self) -> None:
        entries = load_fixture("controlled_listenbrainz_top_recordings.json")
        serialized = json.dumps(entries)
        chunks = [
            serialized[index : index + 7]
            for index in range(0, len(serialized), 7)
        ]
        session = Mock(spec=requests.Session)
        session.get.return_value = stream_response(chunks)
        client = self.client_for(session)

        payload = client.fetch_top_recordings(MBID)

        self.assertEqual(payload, entries)

    def test_a_streamed_body_that_is_not_a_list_is_a_resolution_failure(
        self,
    ) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = stream_response(['{"recording_mbid": "x"}'])
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_top_recordings(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)

    def test_a_truncated_streamed_body_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = stream_response(['[{"recording_mbid": "x"}'])
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_top_recordings(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)

    def test_a_429_propagates_the_retry_after_delay_as_unavailable(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(429, headers={"Retry-After": "7"})
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_similar_artists(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertEqual(raised.exception.retry_after_seconds, 7.0)

    def test_server_failures_are_unavailable_and_retryable(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(503, text="unavailable")
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_similar_artists(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.provider_unavailable)
        self.assertTrue(raised.exception.retryable)
        self.assertIsNone(raised.exception.retry_after_seconds)

    def test_a_404_is_a_typed_source_not_found(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(404, text="not found")
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_similar_artists(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.source_not_found)
        self.assertFalse(raised.exception.retryable)

    def test_a_rejected_request_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(400, text="bad request")
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_similar_artists(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)

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
                client = self.client_for(session)

                with self.assertRaises(DocumentError) as raised:
                    client.fetch_similar_artists(MBID)

                self.assertEqual(
                    raised.exception.code, ErrorCode.provider_unavailable
                )
                self.assertTrue(raised.exception.retryable)
                self.assertNotIn("private", str(raised.exception))

    def test_a_non_json_body_is_a_resolution_failure(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, text="<html>not json</html>")
        client = self.client_for(session)

        with self.assertRaises(DocumentError) as raised:
            client.fetch_similar_artists(MBID)

        self.assertEqual(raised.exception.code, ErrorCode.resolution_failed)
        self.assertFalse(raised.exception.retryable)

    def test_consecutive_calls_are_paced_at_one_per_second(self) -> None:
        session = Mock(spec=requests.Session)
        session.get.return_value = response(200, body=[])
        session.post.return_value = response(200, body=[])
        client = ListenBrainzClient(
            None, USER_AGENT, session=session, minimum_interval=0.05
        )

        started = time.monotonic()
        client.fetch_similar_artists(MBID)
        client.fetch_artist_popularity(MBID)
        elapsed = time.monotonic() - started

        self.assertEqual(session.get.call_count, 1)
        self.assertEqual(session.post.call_count, 1)
        self.assertGreaterEqual(elapsed, 0.05)

    def test_the_token_is_not_logged_or_echoed_in_an_error(self) -> None:
        failures = [
            requests.ConnectionError("connection refused"),
            requests.Timeout("timed out"),
        ]

        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                session = Mock(spec=requests.Session)
                session.get.side_effect = failure
                client = self.client_for(session, token=SENTINEL_TOKEN)
                records = []
                handler = logging.Handler()
                handler.emit = records.append
                root = logging.getLogger()
                root.addHandler(handler)

                try:
                    with self.assertRaises(DocumentError) as raised:
                        client.fetch_similar_artists(MBID)
                finally:
                    root.removeHandler(handler)

                self.assertNotIn(SENTINEL_TOKEN, str(raised.exception))
                self.assertNotIn(
                    SENTINEL_TOKEN,
                    "".join(str(record.getMessage()) for record in records),
                )
                self.assertTrue(raised.exception.__suppress_context__)

    def test_configured_token_and_algorithm_read_the_metadata_environment(
        self,
    ) -> None:
        with patch.dict(
            os.environ,
            {
                LISTENBRAINZ_TOKEN_VARIABLE: "  resolved-token  ",
                LISTENBRAINZ_ALGORITHM_VARIABLE: "  pinned-algorithm-id  ",
            },
        ):
            self.assertEqual(configured_token(), "resolved-token")
            self.assertEqual(configured_algorithm(), "pinned-algorithm-id")

        for value in ["", "   "]:
            with self.subTest(value=value):
                with patch.dict(os.environ, {LISTENBRAINZ_TOKEN_VARIABLE: value}):
                    self.assertIsNone(configured_token())

    def test_the_default_algorithm_is_the_pinned_labs_string(self) -> None:
        with patch.dict(os.environ, {LISTENBRAINZ_ALGORITHM_VARIABLE: ""}):
            self.assertEqual(
                configured_algorithm(),
                "session_based_days_7500_session_300_contribution_5_threshold_10"
                "_limit_100_filter_True_skip_30",
            )


class ListenBrainzArtistMetadataTest(unittest.TestCase):
    def setUp(self) -> None:
        self.source = SourceIdentity(provider_id="listenbrainz", external_id=MBID)

    def client_for(self, algorithm=LISTENBRAINZ_DEFAULT_ALGORITHM) -> Mock:
        client = Mock(spec=ListenBrainzClient)
        client.algorithm = algorithm
        client.fetch_similar_artists.return_value = load_fixture(
            "controlled_listenbrainz_similar_artists.json"
        )
        client.fetch_top_recordings.return_value = load_fixture(
            "controlled_listenbrainz_top_recordings.json"
        )
        client.fetch_artist_popularity.return_value = load_fixture(
            "controlled_listenbrainz_artist_popularity.json"
        )
        return client

    def metadata_for(self, client) -> ListenBrainzArtistMetadata:
        return ListenBrainzArtistMetadata(client)

    def test_one_document_yields_all_four_fields_with_one_call_each(self) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        client.fetch_similar_artists.assert_called_once_with(MBID)
        client.fetch_top_recordings.assert_called_once_with(MBID)
        client.fetch_artist_popularity.assert_called_once_with(MBID)
        self.assertEqual(
            candidate_fields(result),
            ["similar", "top_tracks", "metric.listen", "metric.listener"],
        )
        self.assertEqual(result.errors, [])
        self.assertEqual(result.address, self.source)

    def test_the_similar_ranking_scope_is_the_pinned_algorithm_verbatim(
        self,
    ) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, ["similar"])

        self.assertEqual(
            candidate_for(result, "similar").ranking_scope,
            "session_based_days_7500_session_300_contribution_5_threshold_10"
            "_limit_100_filter_True_skip_30",
        )
        self.assertEqual(
            candidate_for(result, "similar").ranking_scope, client.algorithm
        )

    def test_the_top_tracks_ranking_scope_names_the_endpoint(self) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["top_tracks"]
        )

        self.assertEqual(
            candidate_for(result, "top_tracks").ranking_scope,
            LISTENBRAINZ_TOP_TRACKS_RANKING_SCOPE,
        )

    def test_similar_candidates_keep_the_provider_order(self) -> None:
        entries = load_fixture("controlled_listenbrainz_similar_artists.json")
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, ["similar"])

        candidates = candidate_for(result, "similar").candidates
        self.assertEqual(
            [candidate.name for candidate in candidates],
            [entry["name"] for entry in entries],
        )
        self.assertEqual(
            [candidate.rank for candidate in candidates],
            [0, 1, 2],
        )
        self.assertEqual(
            [candidate.score for candidate in candidates],
            [entry["score"] for entry in entries],
        )
        self.assertEqual(
            [str(candidate.source.external_id) for candidate in candidates],
            [entry["artist_mbid"] for entry in entries],
        )
        self.assertTrue(
            all(
                candidate.source.match_basis == MatchBasis.identifier
                for candidate in candidates
            )
        )

    def test_the_artist_metric_never_comes_from_a_recording_count(self) -> None:
        client = self.client_for()
        popularity = load_fixture(
            "controlled_listenbrainz_artist_popularity.json"
        )[0]
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        listen = candidate_for(result, "metric.listen")
        listener = candidate_for(result, "metric.listener")
        self.assertEqual(listen.count, popularity["total_listen_count"])
        self.assertEqual(listener.count, popularity["total_user_count"])
        for candidate in candidate_for(result, "top_tracks").candidates:
            self.assertNotEqual(candidate.rank, listen.count)
        self.assertEqual(client.fetch_artist_popularity.call_count, 1)

    def test_metric_population_is_present_and_period_is_absent(self) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["metric.listen", "metric.listener"]
        )

        listen = candidate_for(result, "metric.listen")
        listener = candidate_for(result, "metric.listener")
        self.assertEqual(listen.metric.unit, "listens")
        self.assertEqual(listener.metric.unit, "listeners")
        self.assertEqual(listen.metric.population, "ListenBrainz users")
        self.assertEqual(listener.metric.population, "ListenBrainz users")
        self.assertIsNone(listen.metric.period)
        self.assertIsNone(listener.metric.period)

    def test_every_candidate_licence_forbids_redistribution(self) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        for candidate in result.candidates:
            with self.subTest(field=candidate.field):
                self.assertEqual(
                    candidate.licence.id, LISTENBRAINZ_OUTPUT_LICENCE_ID
                )
                self.assertFalse(candidate.licence.redistribution_allowed)
                self.assertTrue(candidate.licence.cache_allowed)
                self.assertTrue(candidate.licence.attribution_required)
                self.assertTrue(candidate.licence.uncertainty)
                self.assertFalse(candidate.licence.share_alike)
                self.assertFalse(candidate.licence.non_commercial)

    def test_recordings_carry_their_own_listen_and_user_counts(self) -> None:
        entries = load_fixture("controlled_listenbrainz_top_recordings.json")
        client = self.client_for()
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["top_tracks"]
        )

        candidates = candidate_for(result, "top_tracks").candidates
        self.assertEqual(
            [candidate.rank for candidate in candidates],
            [entry["total_listen_count"] for entry in entries],
        )
        self.assertEqual(
            [candidate.score for candidate in candidates],
            [float(entry["total_user_count"]) for entry in entries],
        )
        self.assertEqual(
            [str(candidate.source.external_id) for candidate in candidates],
            [entry["recording_mbid"] for entry in entries],
        )

    def test_empty_lists_are_absence_without_an_error(self) -> None:
        client = self.client_for()
        client.fetch_similar_artists.return_value = []
        client.fetch_top_recordings.return_value = []
        client.fetch_artist_popularity.return_value = []
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        self.assertEqual(result.candidates, [])
        self.assertEqual(result.errors, [])

    def test_null_metric_counts_are_absence_without_an_error(self) -> None:
        client = self.client_for()
        client.fetch_artist_popularity.return_value = load_fixture(
            "controlled_listenbrainz_artist_popularity_null_counts.json"
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        self.assertNotIn("metric.listen", candidate_fields(result))
        self.assertNotIn("metric.listener", candidate_fields(result))
        self.assertEqual(result.errors, [])

    def test_a_recording_without_a_title_is_omitted_not_invented(self) -> None:
        entry = load_fixture(
            "controlled_listenbrainz_top_recordings_without_title.json"
        )[0]
        client = self.client_for()
        client.fetch_top_recordings.return_value = [entry]
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["top_tracks"]
        )

        self.assertNotIn("top_tracks", candidate_fields(result))
        self.assertEqual(result.errors, [])

    def test_the_lists_are_truncated_to_the_instance_limit(self) -> None:
        client = self.client_for()
        client.fetch_similar_artists.return_value = load_fixture(
            "controlled_listenbrainz_similar_artists_over_limit.json"
        )
        client.fetch_top_recordings.return_value = load_fixture(
            "controlled_listenbrainz_top_recordings_over_limit.json"
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        self.assertEqual(
            len(candidate_for(result, "similar").candidates),
            INSTANCE_DISCOVERY_ENTRY_LIMIT,
        )
        self.assertEqual(
            len(candidate_for(result, "top_tracks").candidates),
            INSTANCE_DISCOVERY_ENTRY_LIMIT,
        )

    def test_a_failing_similar_call_is_a_field_error_that_spares_the_others(
        self,
    ) -> None:
        client = self.client_for()
        client.fetch_similar_artists.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        self.assertEqual(
            candidate_fields(result),
            ["top_tracks", "metric.listen", "metric.listener"],
        )
        error = error_for(result, "similar")
        self.assertEqual(error.code, ErrorCode.provider_unavailable)
        self.assertTrue(error.retryable)

    def test_a_failing_popularity_call_errors_both_metric_fields(self) -> None:
        client = self.client_for()
        client.fetch_artist_popularity.side_effect = DocumentError(
            ErrorCode.resolution_failed, False
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, None)

        self.assertEqual(candidate_fields(result), ["similar", "top_tracks"])
        self.assertEqual(
            [error.field for error in result.errors],
            ["metric.listen", "metric.listener"],
        )
        self.assertEqual(
            error_for(result, "metric.listen").code,
            ErrorCode.resolution_failed,
        )

    def test_only_the_requested_fields_are_fetched(self) -> None:
        client = self.client_for()
        metadata = self.metadata_for(client)

        metadata.fetch_artist_document(self.source, None, ["similar"])

        client.fetch_similar_artists.assert_called_once_with(MBID)
        client.fetch_top_recordings.assert_not_called()
        client.fetch_artist_popularity.assert_not_called()

    def test_the_requested_fields_filter_candidates_and_errors(self) -> None:
        client = self.client_for()
        client.fetch_artist_popularity.side_effect = DocumentError(
            ErrorCode.provider_unavailable, True
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["metric.listen"]
        )

        self.assertEqual(candidate_fields(result), [])
        self.assertEqual([error.field for error in result.errors], ["metric.listen"])

    def test_a_wrong_typed_body_is_a_resolution_failure(self) -> None:
        payloads = [
            (
                "fetch_similar_artists",
                load_fixture(
                    "controlled_listenbrainz_similar_artists_wrong_type.json"
                ),
            ),
            (
                "fetch_similar_artists",
                load_fixture(
                    "controlled_listenbrainz_similar_artists_wrong_entry_type.json"
                ),
            ),
            (
                "fetch_top_recordings",
                load_fixture(
                    "controlled_listenbrainz_top_recordings_without_title.json"
                )[0],
            ),
            (
                "fetch_artist_popularity",
                load_fixture(
                    "controlled_listenbrainz_artist_popularity_wrong_type.json"
                ),
            ),
            (
                "fetch_artist_popularity",
                load_fixture(
                    "controlled_listenbrainz_artist_popularity_wrong_entry_type.json"
                ),
            ),
        ]

        for member, payload in payloads:
            with self.subTest(member=member, payload=payload):
                client = self.client_for()
                getattr(client, member).return_value = payload
                metadata = self.metadata_for(client)

                result = metadata.fetch_artist_document(
                    self.source, None, [field_for(member)]
                )

                self.assertEqual(result.candidates, [])
                self.assertEqual(
                    error_for(result, field_for(member)).code,
                    ErrorCode.resolution_failed,
                )

    def test_a_wrong_similar_reference_is_a_resolution_failure(self) -> None:
        client = self.client_for()
        client.fetch_similar_artists.return_value = load_fixture(
            "controlled_listenbrainz_similar_artists_wrong_reference.json"
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(self.source, None, ["similar"])

        self.assertEqual(
            error_for(result, "similar").code, ErrorCode.resolution_failed
        )

    def test_a_wrong_popularity_identity_is_a_resolution_failure(self) -> None:
        client = self.client_for()
        client.fetch_artist_popularity.return_value = load_fixture(
            "controlled_listenbrainz_artist_popularity_wrong_identity.json"
        )
        metadata = self.metadata_for(client)

        result = metadata.fetch_artist_document(
            self.source, None, ["metric.listen", "metric.listener"]
        )

        self.assertEqual(
            error_for(result, "metric.listen").code,
            ErrorCode.resolution_failed,
        )
        self.assertEqual(
            error_for(result, "metric.listener").code,
            ErrorCode.resolution_failed,
        )

    def test_no_controlled_fixture_carries_a_credential(self) -> None:
        for path in sorted(FIXTURES.glob("controlled_listenbrainz_*.json")):
            with self.subTest(fixture=path.name):
                text = path.read_text(encoding="utf-8")
                self.assertNotIn(SENTINEL_TOKEN, text)
                self.assertNotIn("Authorization", text)
                self.assertNotIn("token", text.lower())


def field_for(member: str) -> str:
    if member == "fetch_similar_artists":
        return "similar"
    if member == "fetch_top_recordings":
        return "top_tracks"
    return "metric.listen"
