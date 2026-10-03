import json
import os
import threading
import time
import unittest
from dataclasses import dataclass
from pathlib import Path
from unittest.mock import Mock, patch
from urllib.parse import urlparse

import requests

from app.audiodb.client import AudioDbClient
from app.generated.resolver_v1 import SourceIdentity
from app.hosts import HostLimiter
from app.listenbrainz.artist_metadata import ListenBrainzArtistMetadata
from app.listenbrainz.client import (
    LISTENBRAINZ_PROVIDER,
    ListenBrainzClient,
)
from app.metadata_main import configured_sources
from app.musicbrainz.artist_metadata import MusicBrainzArtistMetadata
from app.musicbrainz.client import (
    MUSICBRAINZ_PROVIDER,
    MusicBrainzClient,
)

FIXTURES = Path(__file__).parent / "fixtures" / "metadata"
MBID = "cc197bad-dc9c-440d-a5b5-d52ba2e14234"
USER_AGENT = "whio-resolver/1.0 ( https://example.test/contact )"
WIKIMEDIA_USER_AGENT = "whio-resolver-wikimedia/1.0 ( https://example.test/contact )"
AUDIODB_KEY_VARIABLE = "WHIO_RESOLVER_AUDIODB_KEY"
MUSICBRAINZ_USER_AGENT_VARIABLE = "WHIO_RESOLVER_MUSICBRAINZ_USER_AGENT"
WIKIMEDIA_USER_AGENT_VARIABLE = "WHIO_RESOLVER_WIKIMEDIA_USER_AGENT"
LISTENBRAINZ_TOKEN_VARIABLE = "WHIO_RESOLVER_LISTENBRAINZ_TOKEN"
LISTENBRAINZ_ALGORITHM_VARIABLE = "WHIO_METADATA_ALGORITHM"

INTERVAL_SECONDS = 0.2
LONG_INTERVAL_SECONDS = 60.0
PACING_TOLERANCE_SECONDS = 0.01

CHAIN_PAYLOADS = (
    ("/ws/2/artist/", "controlled_musicbrainz_artist_document.json"),
    ("www.wikidata.org/w/api.php", "controlled_wikidata_entity.json"),
    ("commons.wikimedia.org/w/api.php", "controlled_commons_imageinfo.json"),
    ("api/rest_v1/page/summary/", "controlled_wikipedia_summary.json"),
    ("en.wikipedia.org/w/api.php", "controlled_wikipedia_extract.json"),
)


def load_fixture(name: str):
    with (FIXTURES / name).open(encoding="utf-8") as handle:
        return json.load(handle)


def response(status_code: int = 200, body=None, headers=None):
    result = Mock(spec=requests.Response)
    result.status_code = status_code
    result.headers = headers or {}
    if body is None:
        result.json.side_effect = ValueError("not json")
    else:
        result.json.return_value = body
    return result


@dataclass
class RecordedCall:
    method: str
    url: str
    at: float
    headers: dict


def recording_session(payloads):
    records: list[RecordedCall] = []
    session = Mock(spec=requests.Session)

    def record(method, url, kwargs):
        records.append(
            RecordedCall(
                method,
                url,
                time.monotonic(),
                dict(kwargs.get("headers") or {}),
            )
        )
        for fragment, payload in payloads:
            if fragment in url:
                body = (
                    load_fixture(payload) if isinstance(payload, str) else payload
                )
                return response(200, body=body)
        raise AssertionError(f"unexpected url: {url}")

    session.get.side_effect = lambda url, **kwargs: record("GET", url, kwargs)
    session.post.side_effect = lambda url, **kwargs: record("POST", url, kwargs)
    return session, records


def host_of(url: str) -> str:
    return urlparse(url).netloc


def run_concurrently(calls):
    barrier = threading.Barrier(len(calls))

    def run(call):
        barrier.wait(timeout=10)
        call()

    threads = [threading.Thread(target=run, args=(call,)) for call in calls]

    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()


class HostLimiterConcurrencyTest(unittest.TestCase):
    def test_concurrent_calls_to_one_host_are_serialised_at_the_interval(
        self,
    ) -> None:
        limiter = HostLimiter()
        artist = load_fixture("controlled_musicbrainz_artist_document.json")
        sessions = []
        for _ in range(2):
            session, records = recording_session([("/ws/2/artist/", artist)])
            sessions.append((session, records))
        clients = [
            MusicBrainzClient(
                USER_AGENT,
                session=session,
                musicbrainz_minimum_interval=INTERVAL_SECONDS,
                limiter=limiter,
            )
            for session, _ in sessions
        ]

        run_concurrently(
            [lambda client=client: client.fetch_artist(MBID) for client in clients]
        )

        for session, records in sessions:
            self.assertEqual(session.get.call_count, 1)
            self.assertEqual(len(records), 1)
        starts = sorted(calls[0].at for _, calls in sessions)
        self.assertGreaterEqual(
            starts[1] - starts[0], INTERVAL_SECONDS - PACING_TOLERANCE_SECONDS
        )

    def test_calls_to_different_hosts_are_not_serialised_against_each_other(
        self,
    ) -> None:
        limiter = HostLimiter()
        hosts = ("musicbrainz.org", "api.listenbrainz.org")
        for host in hosts:
            with limiter.serialize(host, 0.0):
                pass
        entered: list[str] = []
        release = threading.Event()

        def blocking_sleep(seconds: float) -> None:
            entered.append(threading.current_thread().name)
            release.wait(timeout=30)

        def serialize(host: str) -> None:
            with limiter.serialize(host, 60.0):
                pass

        with patch("app.hosts.time.sleep", blocking_sleep):
            threads = [
                threading.Thread(target=serialize, args=(host,)) for host in hosts
            ]
            for thread in threads:
                thread.start()
            deadline = time.monotonic() + 5
            while len(entered) < len(hosts) and time.monotonic() < deadline:
                release.wait(0.005)
            overlapped = len(entered)
            release.set()
            for thread in threads:
                thread.join(timeout=5)

        self.assertEqual(overlapped, len(hosts))

    def test_concurrent_calls_to_a_zero_interval_host_hold_one_in_flight(
        self,
    ) -> None:
        limiter = HostLimiter()
        entered: list[float] = []
        release = threading.Event()

        def get(url, **kwargs):
            entered.append(time.monotonic())
            release.wait(timeout=30)
            return response(200, body={"query": {}})

        session = Mock(spec=requests.Session)
        session.get.side_effect = get
        clients = [
            MusicBrainzClient(
                USER_AGENT,
                session=session,
                wikimedia_minimum_interval=0.0,
                limiter=limiter,
            )
            for _ in range(2)
        ]
        barrier = threading.Barrier(2)

        def call(client):
            barrier.wait(timeout=10)
            client.fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")

        threads = [
            threading.Thread(target=call, args=(client,)) for client in clients
        ]
        for thread in threads:
            thread.start()
        deadline = time.monotonic() + 5
        while not entered and time.monotonic() < deadline:
            release.wait(0.005)
        release.wait(0.1)
        concurrent = len(entered)
        release.set()
        for thread in threads:
            thread.join(timeout=10)

        self.assertEqual(session.get.call_count, 2)
        self.assertEqual(concurrent, 1)

    def test_a_recent_call_to_one_host_does_not_delay_a_different_host(
        self,
    ) -> None:
        limiter = HostLimiter()
        artist_session, _ = recording_session(
            [
                (
                    "/ws/2/artist/",
                    load_fixture("controlled_musicbrainz_artist_document.json"),
                )
            ]
        )
        similar_session, _ = recording_session(
            [
                (
                    "/similar-artists/json",
                    load_fixture("controlled_listenbrainz_similar_artists.json"),
                )
            ]
        )
        artist_client = MusicBrainzClient(
            USER_AGENT,
            session=artist_session,
            musicbrainz_minimum_interval=LONG_INTERVAL_SECONDS,
            limiter=limiter,
        )
        similar_client = ListenBrainzClient(
            None,
            USER_AGENT,
            session=similar_session,
            minimum_interval=LONG_INTERVAL_SECONDS,
            limiter=limiter,
        )

        artist_client.fetch_artist(MBID)

        with patch("app.hosts.time.sleep") as sleep:
            similar_client.fetch_similar_artists(MBID)
            different_host_waited = sleep.called
            artist_client.fetch_artist(MBID)
            same_host_waited = sleep.called

        self.assertFalse(different_host_waited)
        self.assertTrue(same_host_waited)

    def test_the_audiodb_budget_is_shared_across_constructed_clients(self) -> None:
        limiter = HostLimiter()
        session, records = recording_session(
            [("/artist-mb.php", {"artists": []})]
        )
        clients = [
            AudioDbClient(
                "configured-key",
                session=session,
                minimum_interval=INTERVAL_SECONDS,
                limiter=limiter,
            )
            for _ in range(2)
        ]

        for client in clients:
            client.fetch_artist_by_mbid(MBID)

        self.assertEqual(len(records), 2)
        self.assertGreaterEqual(
            records[1].at - records[0].at,
            INTERVAL_SECONDS - PACING_TOLERANCE_SECONDS,
        )

    def test_one_chain_request_paces_every_repeated_host_call(self) -> None:
        limiter = HostLimiter()
        session, records = recording_session(CHAIN_PAYLOADS)
        client = MusicBrainzClient(
            USER_AGENT,
            session=session,
            musicbrainz_minimum_interval=INTERVAL_SECONDS,
            wikimedia_minimum_interval=INTERVAL_SECONDS,
            limiter=limiter,
        )
        source = SourceIdentity(provider_id=MUSICBRAINZ_PROVIDER, external_id=MBID)

        document = MusicBrainzArtistMetadata(client).fetch_artist_document(
            source, None, None
        )

        self.assertTrue(document.candidates)
        self.assertEqual(
            {host_of(record.url) for record in records},
            {
                "musicbrainz.org",
                "www.wikidata.org",
                "commons.wikimedia.org",
                "en.wikipedia.org",
            },
        )
        times_by_host: dict[str, list[float]] = {}
        for record in records:
            times_by_host.setdefault(host_of(record.url), []).append(record.at)
        repeated = {
            host: times
            for host, times in times_by_host.items()
            if len(times) > 1
        }
        self.assertIn("en.wikipedia.org", repeated)
        for times in repeated.values():
            for earlier, later in zip(times, times[1:]):
                self.assertGreaterEqual(
                    later - earlier, INTERVAL_SECONDS - PACING_TOLERANCE_SECONDS
                )

    def test_every_chain_and_listenbrainz_call_carries_the_user_agent(self) -> None:
        limiter = HostLimiter()
        session, records = recording_session(CHAIN_PAYLOADS)
        client = MusicBrainzClient(
            USER_AGENT,
            WIKIMEDIA_USER_AGENT,
            session=session,
            musicbrainz_minimum_interval=0.0,
            wikimedia_minimum_interval=0.0,
            limiter=limiter,
        )
        source = SourceIdentity(provider_id=MUSICBRAINZ_PROVIDER, external_id=MBID)
        MusicBrainzArtistMetadata(client).fetch_artist_document(source, None, None)

        listenbrainz_session, listenbrainz_records = recording_session(
            [
                (
                    "/similar-artists/json",
                    load_fixture("controlled_listenbrainz_similar_artists.json"),
                ),
                (
                    "/1/popularity/artist",
                    load_fixture("controlled_listenbrainz_artist_popularity.json"),
                ),
            ]
        )
        listenbrainz_client = ListenBrainzClient(
            "configured-token",
            USER_AGENT,
            session=listenbrainz_session,
            minimum_interval=0.0,
            limiter=limiter,
        )
        ListenBrainzArtistMetadata(listenbrainz_client).fetch_artist_document(
            SourceIdentity(provider_id=LISTENBRAINZ_PROVIDER, external_id=MBID),
            None,
            ["similar", "metric.listen", "metric.listener"],
        )

        expected = {
            "musicbrainz.org": USER_AGENT,
            "www.wikidata.org": WIKIMEDIA_USER_AGENT,
            "commons.wikimedia.org": WIKIMEDIA_USER_AGENT,
            "en.wikipedia.org": WIKIMEDIA_USER_AGENT,
        }
        self.assertEqual(len(records), 5)
        for record in records:
            self.assertEqual(
                record.headers.get("User-Agent"), expected[host_of(record.url)]
            )
        self.assertEqual(len(listenbrainz_records), 2)
        for record in listenbrainz_records:
            self.assertEqual(record.headers.get("User-Agent"), USER_AGENT)


class MetadataEntrypointLimiterTest(unittest.TestCase):
    def test_one_limiter_reaches_every_configured_source(self) -> None:
        limiter = HostLimiter()
        environment = {
            AUDIODB_KEY_VARIABLE: "configured-key",
            MUSICBRAINZ_USER_AGENT_VARIABLE: USER_AGENT,
            WIKIMEDIA_USER_AGENT_VARIABLE: WIKIMEDIA_USER_AGENT,
            LISTENBRAINZ_TOKEN_VARIABLE: "configured-token",
            LISTENBRAINZ_ALGORITHM_VARIABLE: "",
        }

        with patch.dict(os.environ, environment):
            sources = configured_sources(limiter)

        self.assertEqual(
            set(sources),
            {"theaudiodb", MUSICBRAINZ_PROVIDER, LISTENBRAINZ_PROVIDER},
        )
        for provider_id, factory in sources.items():
            with self.subTest(provider_id=provider_id):
                first = factory()
                second = factory()
                self.assertIsNot(first, second)
                self.assertIs(first._client._limiter, limiter)
                self.assertIs(second._client._limiter, limiter)
