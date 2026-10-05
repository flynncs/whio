use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::features::metadata::capabilities::{
    CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
};

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
}

struct StubSource {
    result: Result<Snapshot, CapabilityError>,
    calls: AtomicUsize,
}

impl StubSource {
    fn advertising(providers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            result: Ok(Snapshot::new(
                providers.iter().map(|provider| (*provider).to_owned()),
            )),
            calls: AtomicUsize::new(0),
        })
    }

    fn failing(error: CapabilityError) -> Arc<Self> {
        Arc::new(Self {
            result: Err(error),
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CapabilitySource for StubSource {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        self.result.clone()
    }
}

#[test]
fn the_capability_snapshot_ttl_is_five_minutes() {
    assert_eq!(CAPABILITY_TTL, Duration::from_secs(5 * 60));
}

#[test]
fn a_snapshot_advertises_only_the_providers_it_holds() {
    let snapshot = Snapshot::new(["youtube_music".to_owned(), "theaudiodb".to_owned()]);

    assert!(snapshot.advertised("youtube_music"));
    assert!(snapshot.advertised("theaudiodb"));
    assert!(!snapshot.advertised("musicbrainz"));
    assert!(!snapshot.advertised(""));
    assert_eq!(
        snapshot.providers().collect::<Vec<_>>(),
        vec!["theaudiodb", "youtube_music"]
    );
}

#[test]
fn an_empty_snapshot_advertises_nothing() {
    let snapshot = Snapshot::default();

    assert!(!snapshot.advertised("youtube_music"));
    assert!(snapshot.providers().next().is_none());
}

#[test]
fn enabled_sources_keep_the_configured_order_and_drop_the_absent() {
    let snapshot = Snapshot::new(["youtube_music".to_owned(), "commons".to_owned()]);
    let order = vec![
        "theaudiodb".to_owned(),
        "youtube_music".to_owned(),
        "commons".to_owned(),
        "lastfm".to_owned(),
    ];

    assert_eq!(
        snapshot.enabled_sources(&order),
        vec!["youtube_music".to_owned(), "commons".to_owned()]
    );
}

#[test]
fn enabled_sources_is_empty_when_nothing_is_advertised() {
    let snapshot = Snapshot::new(["listenbrainz".to_owned()]);

    assert!(
        snapshot
            .enabled_sources(&["theaudiodb".to_owned()])
            .is_empty()
    );
    assert!(snapshot.enabled_sources(&[]).is_empty());
}

#[tokio::test]
async fn an_advertised_source_is_accepted_and_an_absent_one_is_rejected() {
    let source = StubSource::advertising(&["youtube_music"]);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);

    assert!(
        snapshot.advertised("youtube_music", at(0)).await,
        "the advertised source is dispatchable"
    );
    assert!(
        !snapshot.advertised("theaudiodb", at(0)).await,
        "an absent source must not be dispatched"
    );
}

#[tokio::test]
async fn validation_names_the_unadvertised_source() {
    let source = StubSource::advertising(&["youtube_music"]);
    let snapshot = CapabilitySnapshot::new(source, CAPABILITY_TTL);

    assert_eq!(snapshot.accept("youtube_music", at(0)).await, Ok(()));
    assert_eq!(
        snapshot.accept("theaudiodb", at(0)).await,
        Err(CapabilityError::Unadvertised {
            provider_id: "theaudiodb".to_owned()
        }),
        "an unadvertised producing source writes nothing"
    );
}

#[tokio::test]
async fn the_snapshot_is_read_once_and_held_within_its_ttl() {
    let source = StubSource::advertising(&["youtube_music"]);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);

    snapshot.current(at(0)).await.expect("the first read");
    snapshot.current(at(60)).await.expect("a held read");

    assert_eq!(
        source.calls(),
        1,
        "the snapshot is not re-read inside its ttl"
    );
    assert!(
        snapshot.held(at(60)).is_some(),
        "the snapshot is held in memory"
    );
}

#[tokio::test]
async fn the_snapshot_is_re_read_once_its_ttl_expires() {
    let source = StubSource::advertising(&["youtube_music"]);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);
    let expiry = CAPABILITY_TTL.as_secs() as i64;

    snapshot.current(at(0)).await.expect("the first read");
    snapshot.current(at(expiry - 1)).await.expect("a held read");
    assert_eq!(source.calls(), 1);

    snapshot.current(at(expiry)).await.expect("an expired read");
    assert_eq!(source.calls(), 2, "the ttl expiry forces a re-read");
    assert!(snapshot.held(at(expiry)).is_some(), "the new value is held");
}

#[tokio::test]
async fn a_moved_back_clock_does_not_serve_a_future_snapshot() {
    let source = StubSource::advertising(&["youtube_music"]);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);

    snapshot.current(at(600)).await.expect("the first read");
    assert!(
        snapshot.held(at(0)).is_none(),
        "a snapshot fetched after the caller's instant is not served"
    );
    snapshot.current(at(0)).await.expect("a re-read");
    assert_eq!(source.calls(), 2);
}

#[tokio::test]
async fn an_unreachable_resolver_reports_the_error_and_holds_nothing() {
    let source = StubSource::failing(CapabilityError::Unavailable);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);

    assert_eq!(
        snapshot.current(at(0)).await,
        Err(CapabilityError::Unavailable)
    );
    assert!(
        snapshot.held(at(0)).is_none(),
        "nothing is cached on failure"
    );
    assert!(!snapshot.advertised("youtube_music", at(0)).await);
    assert!(
        snapshot
            .enabled_sources(&["youtube_music".to_owned()], at(0))
            .await
            .is_empty(),
        "an unreachable resolver enables no source"
    );
    assert_eq!(
        source.calls(),
        3,
        "an unavailable fetch is not held as a snapshot"
    );
}

#[tokio::test]
async fn a_disabled_resolver_reports_disabled() {
    let source = StubSource::failing(CapabilityError::Disabled);
    let snapshot = CapabilitySnapshot::new(source, CAPABILITY_TTL);

    assert_eq!(
        snapshot.current(at(0)).await,
        Err(CapabilityError::Disabled)
    );
}

#[tokio::test]
async fn an_advertisement_without_the_capability_enables_no_source() {
    let source = StubSource::advertising(&[]);
    let snapshot = CapabilitySnapshot::new(source, CAPABILITY_TTL);

    assert!(
        snapshot
            .enabled_sources(&["theaudiodb".to_owned()], at(0))
            .await
            .is_empty(),
        "a source with no handler supplies no field"
    );
    assert_eq!(
        snapshot.accept("theaudiodb", at(0)).await,
        Err(CapabilityError::Unadvertised {
            provider_id: "theaudiodb".to_owned()
        })
    );
}

#[tokio::test]
async fn enabled_sources_preserve_the_order_across_a_ttl_boundary() {
    let source = StubSource::advertising(&["youtube_music", "musicbrainz"]);
    let snapshot = CapabilitySnapshot::new(source.clone(), CAPABILITY_TTL);
    let order = vec!["musicbrainz".to_owned(), "youtube_music".to_owned()];
    let expiry = CAPABILITY_TTL.as_secs() as i64;

    assert_eq!(
        snapshot.enabled_sources(&order, at(0)).await,
        vec!["musicbrainz".to_owned(), "youtube_music".to_owned()]
    );
    assert_eq!(
        snapshot.enabled_sources(&order, at(expiry)).await,
        vec!["musicbrainz".to_owned(), "youtube_music".to_owned()]
    );
    assert_eq!(source.calls(), 2);
}
