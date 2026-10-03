use std::sync::Arc;

use chrono::{Duration, Utc};
use sqlx::PgPool;

use super::{CommitOutcome, MetadataStore, PostgresMetadataStore};
use crate::features::metadata::{
    domain::{
        AbsenceReason, AttemptError, Attribution, BytesState, CandidateItem, FieldId, FieldValue,
        FieldWrite, Freshness, ImageFact, MatchBasis, MetadataEntityKind, MetricFact, Presence,
        StoredFact, SubjectFacts, SubjectKey,
    },
    licences::licence_terms,
};
use crate::features::music_library::{
    EnrichmentGuard,
    domain::{ArtistCredit, ArtistId, EnrichmentSubject, ProviderId, SourceIdentity, SourceScope},
    test_support::{CatalogueWrite, PostgresTrackRepository},
};

const ADAPTER_VERSION: &str = "v1";
const MAX_STORED_BYTES: u64 = 4 * 1024 * 1024;

fn write(field: FieldId, value: FieldValue) -> FieldWrite {
    FieldWrite {
        field,
        source_provider_id: "musicbrainz".to_owned(),
        source_external_id: "artist-1".to_owned(),
        adapter_version: ADAPTER_VERSION.to_owned(),
        language: "en".to_owned(),
        value: Some(value),
        licence_id: Some("musicbrainz".to_owned()),
        licence_revision: Some(1),
        attribution: None,
        source_url: None,
        match_basis: MatchBasis::Identifier,
        operation: "lookup".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        fetched_at: Utc::now(),
    }
}

fn key(subject: &EnrichmentSubject, language: &str) -> SubjectKey {
    SubjectKey::new(
        MetadataEntityKind::Artist,
        *subject.artist_id().as_uuid(),
        subject.identity_revision().to_owned(),
        language.to_owned(),
        ADAPTER_VERSION.to_owned(),
    )
}

fn source(provider_id: &str, scope: SourceScope, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).unwrap(),
        scope,
        external_id.to_owned(),
    )
    .unwrap()
}

async fn artist(
    repository: &PostgresTrackRepository,
    name: &str,
    musicbrainz_artist_id: &str,
) -> ArtistId {
    repository
        .resolve_or_create_artist(
            &ArtistCredit::new(name.to_owned(), Some(musicbrainz_artist_id.to_owned())).unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

async fn subject(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    artist_id: &ArtistId,
) -> EnrichmentSubject {
    let mut transaction = pool.begin().await.unwrap();
    let subject = repository
        .enrichment_subject(&mut transaction, artist_id)
        .await
        .unwrap()
        .expect("the artist exists");
    transaction.commit().await.unwrap();

    subject
}

async fn artist_and_subject(
    pool: &PgPool,
    repository: &PostgresTrackRepository,
    name: &str,
    musicbrainz_artist_id: &str,
) -> (ArtistId, EnrichmentSubject) {
    let artist_id = artist(repository, name, musicbrainz_artist_id).await;
    let subject = subject(pool, repository, &artist_id).await;

    (artist_id, subject)
}

fn store_for(
    pool: &PgPool,
    repository: &Arc<PostgresTrackRepository>,
    max_stored_bytes: u64,
) -> PostgresMetadataStore {
    PostgresMetadataStore::new(
        pool.clone(),
        Arc::clone(repository) as Arc<dyn EnrichmentGuard>,
        max_stored_bytes,
    )
}

async fn read(store: &PostgresMetadataStore, subject: &EnrichmentSubject) -> SubjectFacts {
    store.read_subject(&key(subject, "en")).await.unwrap()
}

fn fact(facts: &SubjectFacts, field: FieldId) -> &StoredFact {
    facts
        .facts
        .iter()
        .find(|stored| stored.field == field)
        .expect("the field has a stored fact")
}

async fn rows(pool: &PgPool, table: &str, artist_id: &ArtistId) -> i64 {
    sqlx::query_scalar::<_, i64>(&format!(
        "SELECT count(*) FROM {table} WHERE entity_kind = 'artist' AND entity_id = $1"
    ))
    .bind(artist_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn each_value_kind_round_trips_and_takes_its_licence_from_the_registry(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Round Trip", "mbid-round-trip").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let mut text = write(
        FieldId::BiographySummary,
        FieldValue::Text("a summary".to_owned()),
    );
    text.attribution = Some(Attribution {
        text: "credited source".to_owned(),
        url: Some("https://credit.example/artist".to_owned()),
    });
    text.source_url = Some("https://source.example/artist".to_owned());

    let mut count = write(FieldId::MetricListeners, FieldValue::Count(4_321));
    count.metric = Some(MetricFact {
        unit: "listeners".to_owned(),
        population: Some("global".to_owned()),
        period: Some("all_time".to_owned()),
        as_of: None,
    });

    let reference = write(
        FieldId::PageUrl,
        FieldValue::Reference("https://page.example/artist".to_owned()),
    );

    let items = vec![CandidateItem {
        name: "Another Artist".to_owned(),
        external_id: Some("mbid-another".to_owned()),
        url: Some("https://another.example/artist".to_owned()),
        rank: Some(1),
        score: Some(0.9),
        artists: Vec::new(),
        release_name: None,
        duration_ms: None,
        isrc: None,
        recording_id: None,
        ranking_scope: None,
    }];
    let candidates = write(FieldId::Similar, FieldValue::Candidates(items.clone()));

    for field_write in [&text, &count, &reference, &candidates] {
        let outcome = store.commit_field(&subject, field_write).await.unwrap();

        assert_eq!(outcome, CommitOutcome::Written);
    }

    let facts = read(&store, &subject).await;

    assert_eq!(facts.facts.len(), 4);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 4);

    let registry_terms = licence_terms("musicbrainz", 1).unwrap();

    for field in [
        FieldId::BiographySummary,
        FieldId::MetricListeners,
        FieldId::PageUrl,
        FieldId::Similar,
    ] {
        assert_eq!(fact(&facts, field).licence, registry_terms);
    }

    let summary = fact(&facts, FieldId::BiographySummary);

    assert_eq!(summary.value, FieldValue::Text("a summary".to_owned()));
    assert_eq!(summary.licence.name, "CC0 1.0 Universal");
    assert_eq!(summary.licence.revision, 1);
    assert_eq!(
        summary.licence.url.as_deref(),
        Some("https://creativecommons.org/publicdomain/zero/1.0/")
    );
    assert!(!summary.licence.attribution_required);
    assert_eq!(
        summary.attribution,
        Some(Attribution {
            text: "credited source".to_owned(),
            url: Some("https://credit.example/artist".to_owned()),
        })
    );
    assert_eq!(summary.source.provider_id, "musicbrainz");
    assert_eq!(summary.source.external_id, "artist-1");
    assert_eq!(summary.source.match_basis, MatchBasis::Identifier);
    assert_eq!(
        summary.source.url.as_deref(),
        Some("https://source.example/artist")
    );
    assert_eq!(summary.operation, "lookup");
    assert!(!summary.uncertainty);
    assert_eq!(summary.bytes, None);
    assert!(summary.stored_bytes > 0);

    let listeners = fact(&facts, FieldId::MetricListeners);

    assert_eq!(listeners.value, FieldValue::Count(4_321));
    assert_eq!(
        listeners.metric,
        Some(MetricFact {
            unit: "listeners".to_owned(),
            population: Some("global".to_owned()),
            period: Some("all_time".to_owned()),
            as_of: None,
        })
    );

    assert_eq!(
        fact(&facts, FieldId::PageUrl).value,
        FieldValue::Reference("https://page.example/artist".to_owned())
    );
    assert_eq!(
        fact(&facts, FieldId::Similar).value,
        FieldValue::Candidates(items)
    );

    let now = Utc::now();

    for field in [
        FieldId::BiographySummary,
        FieldId::MetricListeners,
        FieldId::PageUrl,
        FieldId::Similar,
    ] {
        let entry = facts.entry(field, now);

        assert_eq!(entry.presence, Presence::Present);
        assert_eq!(entry.freshness, Freshness::Fresh);
        assert_eq!(entry.absence, None);
    }
}

#[sqlx::test]
async fn a_failure_on_a_third_field_preserves_two_committed_fields(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Partial", "mbid-partial").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let summary = write(
        FieldId::BiographySummary,
        FieldValue::Text("kept summary".to_owned()),
    );

    let mut page = write(
        FieldId::PageUrl,
        FieldValue::Reference("https://page.example/kept".to_owned()),
    );
    page.source_provider_id = "lastfm".to_owned();
    page.source_external_id = "artist-lastfm".to_owned();
    page.licence_id = Some("lastfm".to_owned());

    assert_eq!(
        store.commit_field(&subject, &summary).await.unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(
        store.commit_field(&subject, &page).await.unwrap(),
        CommitOutcome::Written
    );

    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 2);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 2);

    let error = AttemptError {
        class: "transport".to_owned(),
        message: "the provider timed out".to_owned(),
    };
    store
        .record_failure(&subject, FieldId::Tag, "en", "theaudiodb", &error)
        .await
        .unwrap();

    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 2);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 3);

    let facts = read(&store, &subject).await;

    assert_eq!(facts.facts.len(), 2);
    assert_eq!(
        fact(&facts, FieldId::BiographySummary).value,
        FieldValue::Text("kept summary".to_owned())
    );
    assert_eq!(
        fact(&facts, FieldId::PageUrl).value,
        FieldValue::Reference("https://page.example/kept".to_owned())
    );
    assert!(
        facts
            .attempts
            .iter()
            .filter(|attempt| attempt.field != FieldId::Tag)
            .all(|attempt| attempt.error.is_none())
    );

    let failed = facts
        .attempts
        .iter()
        .filter(|attempt| attempt.field == FieldId::Tag)
        .collect::<Vec<_>>();

    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].source_provider_id, "theaudiodb");
    assert_eq!(failed[0].fetched_at, None);
    assert_eq!(failed[0].error, Some(error));

    let now = Utc::now();
    let tag = facts.entry(FieldId::Tag, now);

    assert_eq!(tag.presence, Presence::Absent);
    assert_eq!(tag.absence, None);
    assert_eq!(
        tag.freshness,
        Freshness::Failed("the provider timed out".to_owned())
    );

    let kept = facts.entry(FieldId::BiographySummary, now);

    assert_eq!(kept.presence, Presence::Present);
    assert_eq!(kept.freshness, Freshness::Fresh);
}

#[sqlx::test]
async fn the_four_read_states_follow_the_attempt_and_the_fetched_age(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Read States", "mbid-read-states").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);
    let now = chrono::DateTime::from_timestamp_micros(Utc::now().timestamp_micros()).unwrap();

    let mut reported_none = write(
        FieldId::BiographyLanguage,
        FieldValue::Text("unused".to_owned()),
    );
    reported_none.value = None;
    reported_none.fetched_at = now - Duration::hours(1);

    assert_eq!(
        store.commit_field(&subject, &reported_none).await.unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 0);

    store
        .record_failure(
            &subject,
            FieldId::Tag,
            "en",
            "theaudiodb",
            &AttemptError {
                class: "provider_error".to_owned(),
                message: "the provider failed".to_owned(),
            },
        )
        .await
        .unwrap();

    let mut stale = write(
        FieldId::BiographySummary,
        FieldValue::Text("stale summary".to_owned()),
    );
    stale.fetched_at = now - Duration::days(30);

    assert_eq!(
        store.commit_field(&subject, &stale).await.unwrap(),
        CommitOutcome::Written
    );

    let facts = read(&store, &subject).await;

    let language = facts.entry(FieldId::BiographyLanguage, now);

    assert_eq!(language.presence, Presence::Absent);
    assert_eq!(language.value, None);
    assert_eq!(language.source, None);
    assert_eq!(language.absence, Some(AbsenceReason::ProviderReportedNone));
    assert_eq!(language.freshness, Freshness::Fresh);
    assert_eq!(
        language
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.error.as_ref()),
        None
    );
    assert_eq!(
        language
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.fetched_at),
        Some(now - Duration::hours(1))
    );

    let tag = facts.entry(FieldId::Tag, now);

    assert_eq!(tag.presence, Presence::Absent);
    assert_eq!(tag.absence, None);
    assert_eq!(
        tag.freshness,
        Freshness::Failed("the provider failed".to_owned())
    );

    let untouched = facts.entry(FieldId::BiographyContent, now);

    assert_eq!(untouched.presence, Presence::Absent);
    assert_eq!(untouched.absence, None);
    assert_eq!(untouched.freshness, Freshness::Unrequested);
    assert_eq!(untouched.attempt, None);

    let summary = facts.entry(FieldId::BiographySummary, now);

    assert_eq!(summary.presence, Presence::Present);
    assert_eq!(
        summary.value,
        Some(FieldValue::Text("stale summary".to_owned()))
    );
    assert_eq!(summary.freshness, Freshness::Stale);
    assert_eq!(summary.absence, None);
}

#[sqlx::test]
async fn a_changed_basis_makes_a_stale_subject_obsolete(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, snapshot) =
        artist_and_subject(&pool, &repository, "Obsolete", "mbid-obsolete").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    repository
        .set_artist_source(
            &artist_id,
            &source("local", SourceScope::Named("/music".to_owned()), "track-1"),
        )
        .await
        .unwrap();

    let current = subject(&pool, &repository, &artist_id).await;

    assert_ne!(snapshot.identity_revision(), current.identity_revision());

    let outcome = store
        .commit_field(
            &snapshot,
            &write(
                FieldId::BiographySummary,
                FieldValue::Text("too late".to_owned()),
            ),
        )
        .await
        .unwrap();

    assert_eq!(outcome, CommitOutcome::Obsolete);
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 0);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 0);
    assert!(read(&store, &snapshot).await.facts.is_empty());
    assert!(read(&store, &current).await.facts.is_empty());
}

#[sqlx::test]
async fn a_deleted_artist_is_gone_and_delete_subject_facts_reports_its_rows(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (gone_id, gone) =
        artist_and_subject(&pool, &repository, "Vanishing", "mbid-vanishing").await;
    let (kept_id, kept) = artist_and_subject(&pool, &repository, "Kept", "mbid-kept").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    sqlx::query("DELETE FROM artists WHERE id = $1")
        .bind(gone_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let outcome = store
        .commit_field(
            &gone,
            &write(
                FieldId::BiographySummary,
                FieldValue::Text("never stored".to_owned()),
            ),
        )
        .await
        .unwrap();

    assert_eq!(outcome, CommitOutcome::SubjectGone);
    assert_eq!(rows(&pool, "metadata_facts", &gone_id).await, 0);
    assert_eq!(rows(&pool, "metadata_attempts", &gone_id).await, 0);

    assert_eq!(
        store
            .commit_field(
                &kept,
                &write(
                    FieldId::BiographySummary,
                    FieldValue::Text("kept".to_owned()),
                ),
            )
            .await
            .unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(rows(&pool, "metadata_facts", &kept_id).await, 1);
    assert_eq!(rows(&pool, "metadata_attempts", &kept_id).await, 1);

    let removed = store
        .delete_subject_facts(MetadataEntityKind::Artist, &[*kept_id.as_uuid()])
        .await
        .unwrap();

    assert_eq!(removed, 2);
    assert_eq!(rows(&pool, "metadata_facts", &kept_id).await, 0);
    assert_eq!(rows(&pool, "metadata_attempts", &kept_id).await, 0);
    assert_eq!(
        store
            .delete_subject_facts(MetadataEntityKind::Artist, &[])
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test]
async fn a_commit_over_the_store_budget_keeps_the_stored_fact_and_records_an_attempt(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Budget", "mbid-budget").await;
    let store = store_for(&pool, &repository, 4_096);

    let kept = write(
        FieldId::BiographySummary,
        FieldValue::Text("kept".to_owned()),
    );

    assert_eq!(
        store.commit_field(&subject, &kept).await.unwrap(),
        CommitOutcome::Written
    );

    let before = read(&store, &subject).await;

    assert_eq!(before.facts.len(), 1);

    let oversized = write(
        FieldId::BiographyContent,
        FieldValue::Text("x".repeat(8_192)),
    );

    assert_eq!(
        store.commit_field(&subject, &oversized).await.unwrap(),
        CommitOutcome::Written
    );

    let facts = read(&store, &subject).await;

    assert_eq!(facts.facts.len(), 1);
    assert!(
        !facts
            .facts
            .iter()
            .any(|stored| stored.field == FieldId::BiographyContent)
    );

    let untouched = fact(&facts, FieldId::BiographySummary);

    assert_eq!(untouched.value, before.facts[0].value);
    assert_eq!(untouched.stored_bytes, before.facts[0].stored_bytes);
    assert_eq!(untouched.fetched_at, before.facts[0].fetched_at);
    assert_eq!(facts.attempts.len(), 2);

    let attempt = facts
        .attempts
        .iter()
        .find(|attempt| attempt.field == FieldId::BiographyContent)
        .expect("the oversized field has an attempt");

    assert_eq!(
        attempt.error.as_ref().map(|error| error.class.as_str()),
        Some("store_budget_exceeded")
    );
    assert_eq!(attempt.fetched_at, None);

    let entry = facts.entry(FieldId::BiographyContent, Utc::now());

    assert_eq!(entry.presence, Presence::Absent);
    assert_eq!(entry.absence, None);
    assert!(matches!(entry.freshness, Freshness::Failed(_)));
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 1);
}

#[sqlx::test]
async fn a_large_fact_larger_than_half_the_budget_re_commits_in_place(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Recommit", "mbid-recommit").await;
    let store = store_for(&pool, &repository, 8_192);

    let value = FieldValue::Text("y".repeat(5_000));
    let first = write(FieldId::BiographyContent, value.clone());

    assert_eq!(
        store.commit_field(&subject, &first).await.unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 1);

    let before = read(&store, &subject).await;
    let before_row = fact(&before, FieldId::BiographyContent);

    assert!(
        before_row.stored_bytes > 4_096,
        "the fact is larger than half the 8192-byte budget: {}",
        before_row.stored_bytes
    );

    let mut again = write(FieldId::BiographyContent, value);
    again.fetched_at = before_row.fetched_at + Duration::seconds(60);

    assert_eq!(
        store.commit_field(&subject, &again).await.unwrap(),
        CommitOutcome::Written
    );

    let after = read(&store, &subject).await;

    assert_eq!(
        rows(&pool, "metadata_facts", &artist_id).await,
        1,
        "an identical re-commit updates the row rather than duplicating it"
    );
    assert_eq!(after.facts.len(), 1);

    let after_row = fact(&after, FieldId::BiographyContent);

    assert_eq!(after_row.value, before_row.value);
    assert_eq!(after_row.stored_bytes, before_row.stored_bytes);
    assert!(
        after_row.fetched_at > before_row.fetched_at,
        "the re-commit advances fetched_at: {} -> {}",
        before_row.fetched_at,
        after_row.fetched_at
    );
    assert_eq!(
        after_row
            .terms_basis
            .as_deref()
            .map(str::to_owned)
            .unwrap_or_default(),
        String::new()
    );
}

#[sqlx::test]
async fn a_re_commit_that_grows_past_the_budget_is_still_refused(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Growing", "mbid-growing").await;
    let store = store_for(&pool, &repository, 65_536);

    let first = write(
        FieldId::BiographyContent,
        FieldValue::Text("z".repeat(4_000)),
    );

    assert_eq!(
        store.commit_field(&subject, &first).await.unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 1);

    let before = read(&store, &subject).await;
    let before_row = fact(&before, FieldId::BiographyContent);

    assert!(
        before_row.stored_bytes < 65_536,
        "the first fact fits the budget: {}",
        before_row.stored_bytes
    );

    let mut grown = write(
        FieldId::BiographyContent,
        FieldValue::Text("z".repeat(80_000)),
    );
    grown.fetched_at = before_row.fetched_at + Duration::seconds(60);

    assert_eq!(
        store.commit_field(&subject, &grown).await.unwrap(),
        CommitOutcome::Written
    );

    let after = read(&store, &subject).await;

    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 1);

    let after_row = fact(&after, FieldId::BiographyContent);

    assert_eq!(
        after_row.value, before_row.value,
        "a re-commit whose delta exceeds the budget keeps the stored fact"
    );
    assert_eq!(after_row.fetched_at, before_row.fetched_at);

    let attempt = after
        .attempts
        .iter()
        .find(|attempt| attempt.field == FieldId::BiographyContent)
        .expect("the refused commit records an attempt");

    assert_eq!(
        attempt.error.as_ref().map(|error| error.class.as_str()),
        Some("store_budget_exceeded")
    );
}

#[sqlx::test]
async fn a_commons_licence_is_resolved_from_the_registry_for_image_bytes(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Commons", "mbid-commons").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let mut portrait = write(
        FieldId::ImagePortrait,
        FieldValue::Reference("https://upload.example/portrait.jpg".to_owned()),
    );
    portrait.licence_id = Some("commons:CC BY-SA 2.0".to_owned());
    portrait.licence_revision = Some(0);
    portrait.image = Some(ImageFact {
        role: "portrait".to_owned(),
        width: Some(640),
        height: Some(640),
        content_type: "image/jpeg".to_owned(),
    });

    assert_eq!(
        store.commit_field(&subject, &portrait).await.unwrap(),
        CommitOutcome::Written
    );

    let facts = read(&store, &subject).await;
    let stored = fact(&facts, FieldId::ImagePortrait);
    let terms = licence_terms("commons:CC BY-SA 2.0", 0).unwrap();

    assert_eq!(stored.licence, terms);
    assert_eq!(stored.licence.name, "CC BY-SA 2.0");
    assert_eq!(stored.licence.revision, 0);
    assert!(stored.licence.redistribution_allowed);
    assert_eq!(stored.bytes, Some(BytesState::Servable));
    assert_eq!(
        stored.image,
        Some(ImageFact {
            role: "portrait".to_owned(),
            width: Some(640),
            height: Some(640),
            content_type: "image/jpeg".to_owned(),
        })
    );
    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 1);
}

#[sqlx::test]
async fn the_same_field_under_two_languages_is_two_facts_and_two_entries(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Bilingual", "mbid-bilingual").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let english = write(
        FieldId::BiographySummary,
        FieldValue::Text("english summary".to_owned()),
    );
    let mut french = write(
        FieldId::BiographySummary,
        FieldValue::Text("french summary".to_owned()),
    );
    french.language = "fr".to_owned();

    assert_eq!(
        store.commit_field(&subject, &english).await.unwrap(),
        CommitOutcome::Written
    );
    assert_eq!(
        store.commit_field(&subject, &french).await.unwrap(),
        CommitOutcome::Written
    );

    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 2);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 2);

    let english_facts = store.read_subject(&key(&subject, "en")).await.unwrap();
    let french_facts = store.read_subject(&key(&subject, "fr")).await.unwrap();

    assert_eq!(english_facts.key.language, "en");
    assert_eq!(french_facts.key.language, "fr");
    assert_eq!(english_facts.facts.len(), 1);
    assert_eq!(french_facts.facts.len(), 1);
    assert_eq!(
        fact(&english_facts, FieldId::BiographySummary).value,
        FieldValue::Text("english summary".to_owned())
    );
    assert_eq!(
        fact(&french_facts, FieldId::BiographySummary).value,
        FieldValue::Text("french summary".to_owned())
    );

    let now = Utc::now();
    let english_entry = english_facts.entry(FieldId::BiographySummary, now);
    let french_entry = french_facts.entry(FieldId::BiographySummary, now);

    assert_eq!(english_entry.presence, Presence::Present);
    assert_eq!(french_entry.presence, Presence::Present);
    assert_eq!(
        english_entry.value,
        Some(FieldValue::Text("english summary".to_owned()))
    );
    assert_eq!(
        french_entry.value,
        Some(FieldValue::Text("french summary".to_owned()))
    );
    assert_ne!(english_entry.value, french_entry.value);
}

#[sqlx::test]
async fn two_fields_of_one_subject_commit_concurrently(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) =
        artist_and_subject(&pool, &repository, "Concurrent", "mbid-concurrent").await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let summary = write(
        FieldId::BiographySummary,
        FieldValue::Text("concurrent summary".to_owned()),
    );
    let page = write(
        FieldId::PageUrl,
        FieldValue::Reference("https://page.example/concurrent".to_owned()),
    );

    let (first, second) = tokio::join!(
        store.commit_field(&subject, &summary),
        store.commit_field(&subject, &page),
    );

    assert_eq!(first.unwrap(), CommitOutcome::Written);
    assert_eq!(second.unwrap(), CommitOutcome::Written);

    let facts = read(&store, &subject).await;

    assert_eq!(facts.facts.len(), 2);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 2);
    assert_eq!(
        fact(&facts, FieldId::BiographySummary).value,
        FieldValue::Text("concurrent summary".to_owned())
    );
    assert_eq!(
        fact(&facts, FieldId::PageUrl).value,
        FieldValue::Reference("https://page.example/concurrent".to_owned())
    );
}

#[sqlx::test]
async fn an_unknown_licence_is_rejected_at_commit_and_writes_nothing(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let (artist_id, subject) = artist_and_subject(
        &pool,
        &repository,
        "Unknown Licence",
        "mbid-unknown-licence",
    )
    .await;
    let store = store_for(&pool, &repository, MAX_STORED_BYTES);

    let mut unknown_id = write(
        FieldId::BiographyContent,
        FieldValue::Text("text".to_owned()),
    );
    unknown_id.licence_id = Some("not-a-registered-licence".to_owned());
    unknown_id.licence_revision = Some(1);

    assert!(store.commit_field(&subject, &unknown_id).await.is_err());

    let mut unknown_revision = write(
        FieldId::BiographyContent,
        FieldValue::Text("text".to_owned()),
    );
    unknown_revision.licence_revision = Some(99);

    assert!(
        store
            .commit_field(&subject, &unknown_revision)
            .await
            .is_err()
    );

    assert_eq!(rows(&pool, "metadata_facts", &artist_id).await, 0);
    assert_eq!(rows(&pool, "metadata_attempts", &artist_id).await, 0);
}
