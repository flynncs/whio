use super::PostgresTrackRepository;
use std::sync::Arc;

use crate::features::music_library::domain::{
    AlbumListFilter, AlbumListScope, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistCredit,
    ArtistListFilter, ArtistSort, ArtworkRef, IdSet, KeepReason, MatchQuality, ProviderId, Range,
    SelectorText, SourceIdentity, SourceScope, StateTarget, TrackId, TrackMetadata,
};
use crate::features::music_library::library_state::test_support::seed_play;
use crate::features::music_library::sources::SourceQueries;
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, DuplicateReport, LibraryRead, Retention, SourceFileFacts, SourceLookup,
    StateStore, TrackRepositoryError,
};
use sqlx::PgPool;
use tokio::sync::Barrier;
use uuid::Uuid;

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Named("test-installation".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn genre_set(values: &[&str]) -> IdSet<SelectorText> {
    IdSet::new(
        values
            .iter()
            .map(|value| SelectorText::new(value).unwrap().unwrap()),
    )
    .unwrap()
}

fn year_range(from: i32, to: i32) -> Range<i32> {
    Range::new(Some(from), Some(to)).unwrap()
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str, artists: &[&str], duration_ms: Option<u64>) -> TrackMetadata {
    TrackMetadata::from_names(
        title.to_owned(),
        artists.iter().map(|artist| (*artist).to_owned()).collect(),
        duration_ms,
    )
    .unwrap()
}

#[sqlx::test]

async fn repeated_discovery_preserves_identity_and_updates_metadata(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("round-trip-source");

    let first = repository
        .get_or_create(
            source.clone(),
            metadata("Old title", &["First", "Second"], Some(1_000)),
        )
        .await
        .unwrap();
    let updated = repository
        .get_or_create(
            source.clone(),
            metadata("New title", &["Replacement"], Some(2_000)),
        )
        .await
        .unwrap();

    assert_eq!(first.id(), updated.id());
    assert_eq!(updated.title(), "New title");
    assert_eq!(
        updated
            .artists()
            .iter()
            .map(|artist| artist.name())
            .collect::<Vec<_>>(),
        ["Replacement"]
    );
    assert_eq!(updated.duration_ms(), Some(2_000));
    assert_eq!(
        repository.find_sources(updated.id()).await.unwrap(),
        vec![source]
    );

    let stored_track: (String, Option<i64>) =
        sqlx::query_as("SELECT title, duration_ms FROM tracks WHERE id = $1")
            .bind(updated.id().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    let stored_credits: Vec<(i32, String)> = sqlx::query_as(
        "SELECT position, name FROM track_artist_credits WHERE track_id = $1 ORDER BY position",
    )
    .bind(updated.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(stored_track, ("New title".to_owned(), Some(2_000)));
    assert_eq!(stored_credits, vec![(0, "Replacement".to_owned())]);
}

#[sqlx::test]

async fn simultaneous_discovery_produces_one_track_without_orphans(pool: PgPool) {
    const DISCOVERY_COUNT: usize = 8;

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(DISCOVERY_COUNT));
    let mut tasks = Vec::with_capacity(DISCOVERY_COUNT);

    for observation in 0..DISCOVERY_COUNT {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);

        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .get_or_create(
                    source("concurrent-source"),
                    metadata(&format!("Observation {observation}"), &["Artist"], None),
                )
                .await
        }));
    }

    let mut track_ids = Vec::with_capacity(DISCOVERY_COUNT);
    for task in tasks {
        track_ids.push(task.await.unwrap().unwrap().id().clone());
    }

    assert!(track_ids.iter().all(|track_id| track_id == &track_ids[0]));

    let track_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track_sources")
        .fetch_one(&pool)
        .await
        .unwrap();
    let orphan_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM tracks AS tracks
        LEFT JOIN track_sources AS sources ON sources.track_id = tracks.id
        WHERE sources.track_id IS NULL
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(track_count, 1);
    assert_eq!(source_count, 1);
    assert_eq!(orphan_count, 0);
}

#[sqlx::test]

async fn malformed_stored_scope_is_rejected(pool: PgPool) {
    let track_id = TrackId::new();

    sqlx::query("INSERT INTO tracks (id, title) VALUES ($1, $2)")
        .bind(track_id.as_uuid())
        .bind("Title")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind("youtube_music")
    .bind("not-a-valid-scope")
    .bind("malformed-source")
    .execute(&pool)
    .await
    .unwrap();

    let repository = PostgresTrackRepository::new(pool);
    let error = repository.find_sources(&track_id).await.unwrap_err();

    assert!(matches!(error, TrackRepositoryError::InvalidData));
}

#[sqlx::test]

async fn stored_track_round_trips_through_find_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let stored = repository
        .get_or_create(
            source("detail-source"),
            metadata("Detail title", &["First", "Second"], Some(3_000)),
        )
        .await
        .unwrap();

    let found = repository
        .find_track(stored.id())
        .await
        .unwrap()
        .expect("stored track is found");

    assert_eq!(found, stored);
    assert!(
        repository
            .find_track(&TrackId::new())
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]

async fn artwork_follows_its_source(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("metadata-source");

    let track = repository
        .get_or_create(source.clone(), metadata("Song", &["Artist"], None))
        .await
        .unwrap();
    let refreshed = repository
        .get_or_create(source.clone(), metadata("Song", &["Artist"], Some(1_000)))
        .await
        .unwrap();
    assert_eq!(refreshed.id(), track.id());
    assert_eq!(refreshed.duration_ms(), Some(1_000));

    repository
        .set_source_artwork(
            &source,
            &ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap(),
        )
        .await
        .unwrap();

    let (owner, artwork) = repository
        .find_artwork(track.id())
        .await
        .unwrap()
        .expect("artwork is stored");
    assert_eq!(owner.provider_id().as_str(), "youtube_music");
    assert_eq!(
        artwork,
        ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap()
    );
}

#[sqlx::test]

async fn source_file_facts_round_trip_for_a_local_file(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/tank/data/media/music".to_owned()),
        "11111111-2222-3333-4444-555555555555".to_owned(),
    )
    .unwrap();
    repository
        .get_or_create(source.clone(), metadata("Song", &["Artist"], Some(1_000)))
        .await
        .unwrap();

    let facts = SourceFileFacts {
        path: Some("Artist/Album/01.flac".to_owned()),
        size_bytes: 42,
        modified_at: Some("2026-09-19T00:00:00Z".parse().unwrap()),
        missing_at: None,
        content_type: Some("audio/flac".to_owned()),
        suffix: Some("flac".to_owned()),
        container: Some("flac".to_owned()),
        codec: Some("flac".to_owned()),
        bitrate_kbps: Some(900),
        sample_rate_hz: Some(44_100),
        bit_depth: Some(16),
        channels: Some(2),
        facts_version: 1,
        acoustid_fingerprint: Some("AQAA-test".to_owned()),
    };
    repository
        .set_source_file_facts(&source, &facts)
        .await
        .unwrap();

    assert_eq!(
        repository.find_source_file_facts(&source).await.unwrap(),
        Some(facts.clone())
    );
    assert_eq!(
        repository
            .list_scanned_paths(source.provider_id(), source.source_scope())
            .await
            .unwrap(),
        vec![(source, facts)]
    );
}

#[sqlx::test]

async fn a_provider_source_records_no_local_facts(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("video-without-facts");
    repository
        .get_or_create(source.clone(), metadata("Song", &["Artist"], Some(1_000)))
        .await
        .unwrap();

    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64, i16)>(
        r#"
        SELECT count(container)::BIGINT,
               count(codec)::BIGINT,
               count(bitrate_kbps)::BIGINT,
               count(sample_rate_hz)::BIGINT,
               count(bit_depth)::BIGINT,
               count(channels)::BIGINT,
               min(facts_version)
        FROM track_sources
        WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3
        "#,
    )
    .bind("youtube_music")
    .bind("named:test-installation")
    .bind("video-without-facts")
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        row,
        (0, 0, 0, 0, 0, 0, 0),
        "a provider row states none of the local representation facts"
    );
}

#[sqlx::test]

async fn recorded_local_file_exposes_the_stored_facts(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let source = local_source("recording-1.flac");
    let mut stored = facts("recording-1.flac", 42, "flac");
    stored.content_type = Some("audio/flac".to_owned());
    stored.container = Some("flac".to_owned());
    stored.codec = Some("flac".to_owned());
    stored.bitrate_kbps = Some(900);
    stored.sample_rate_hz = Some(44_100);
    stored.bit_depth = Some(16);
    stored.channels = Some(2);
    repository
        .index_source(
            source.clone(),
            metadata("Song", &["Artist"], Some(1_000)),
            &stored,
        )
        .await
        .unwrap();

    let queries = SourceQueries::new(Arc::clone(&repository) as Arc<dyn SourceLookup>);
    let recorded = queries
        .recorded_local_file(&source)
        .await
        .unwrap()
        .expect("the file is recorded");

    assert_eq!(recorded.path.as_deref(), Some("recording-1.flac"));
    assert!(!recorded.missing);
    assert_eq!(recorded.content_type.as_deref(), Some("audio/flac"));
    assert_eq!(recorded.suffix.as_deref(), Some("flac"));
    assert_eq!(recorded.container.as_deref(), Some("flac"));
    assert_eq!(recorded.codec.as_deref(), Some("flac"));
    assert_eq!(recorded.bitrate_kbps, Some(900));
    assert_eq!(recorded.sample_rate_hz, Some(44_100));
    assert_eq!(recorded.bit_depth, Some(16));
    assert_eq!(recorded.channels, Some(2));
}

fn credit(name: &str, identifier: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), identifier.map(str::to_owned)).unwrap()
}

fn album(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
}

async fn insert_user(pool: &PgPool, id: Uuid, username: &str) {
    sqlx::query("INSERT INTO users (id, username, display_name) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(username)
        .bind(username)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn a_name_only_credit_joins_one_identified_artist(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let identified = repository
        .resolve_or_create_artist(&credit("beabadoobee", Some("mb-bea")))
        .await
        .unwrap();

    let provider = repository
        .resolve_or_create_artist(&credit("beabadoobee", None))
        .await
        .unwrap();

    assert_eq!(provider.id(), identified.id());
    assert_eq!(provider.musicbrainz_artist_id(), Some("mb-bea"));
}

#[sqlx::test]
async fn a_name_only_credit_does_not_choose_between_same_named_artists(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let identified = repository
        .resolve_or_create_artist(&credit("beabadoobee", Some("mb-bea")))
        .await
        .unwrap();
    repository
        .resolve_or_create_artist(&credit("beabadoobee", Some("mb-bea-alt")))
        .await
        .unwrap();

    let provider = repository
        .resolve_or_create_artist(&credit("beabadoobee", None))
        .await
        .unwrap();

    assert_ne!(provider.id(), identified.id());
    assert_eq!(provider.musicbrainz_artist_id(), None);
}

#[sqlx::test]

async fn artist_ladder_is_identifier_then_normalised_name(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let named = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    let again = repository
        .resolve_or_create_artist(&credit("  daft   punk ", None))
        .await
        .unwrap();
    assert_eq!(named.id(), again.id());

    let attached = repository
        .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
        .await
        .unwrap();
    assert_eq!(attached.id(), named.id(), "an id attaches to the name row");
    assert_eq!(attached.musicbrainz_artist_id(), Some("mb-1"));

    let plain = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    assert_eq!(
        plain.id(),
        attached.id(),
        "a unique normalised name resolves to the identified artist"
    );
}

#[sqlx::test]

async fn typographic_variants_resolve_to_one_artist(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let curly = repository
        .resolve_or_create_artist(&credit("Don\u{2019}t Stop", None))
        .await
        .unwrap();
    let straight = repository
        .resolve_or_create_artist(&credit("Don't Stop", None))
        .await
        .unwrap();
    assert_eq!(curly.id(), straight.id());

    let en_dash = repository
        .resolve_or_create_artist(&credit("Sun \u{2013} Ra", None))
        .await
        .unwrap();
    let hyphen = repository
        .resolve_or_create_artist(&credit("Sun - Ra", None))
        .await
        .unwrap();
    assert_eq!(en_dash.id(), hyphen.id());
}

#[sqlx::test]

async fn typographic_variants_resolve_to_one_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let artist = credit("Cage the Elephant", None);

    let curly = repository
        .resolve_or_create_album(
            &album("It\u{2019}s Just Forever").with_album_artist(artist.clone()),
        )
        .await
        .unwrap();
    let straight = repository
        .resolve_or_create_album(&album("It's Just Forever").with_album_artist(artist))
        .await
        .unwrap();
    assert_eq!(curly.id(), straight.id());
}

#[sqlx::test]
async fn a_text_album_joins_one_album_with_a_release_group(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let identified = repository
        .resolve_or_create_album(
            &album("Beatopia")
                .with_album_artist(credit("beabadoobee", Some("mb-bea")))
                .with_release_group_id(Some("rg-beatopia".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    let provider = repository
        .resolve_or_create_album(&album("Beatopia").with_album_artist(credit("beabadoobee", None)))
        .await
        .unwrap();

    assert_eq!(provider.id(), identified.id());
}

#[sqlx::test]
async fn a_text_album_chooses_the_only_edition_without_a_release_group(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let artist = credit("Daft Punk", None);
    let plain = repository
        .resolve_or_create_album(&album("Discovery").with_album_artist(artist.clone()))
        .await
        .unwrap();
    repository
        .resolve_or_create_album(
            &album("Discovery")
                .with_album_artist(artist.clone())
                .with_release_group_id(Some("rg-remaster".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    let provider = repository
        .resolve_or_create_album(&album("Discovery").with_album_artist(artist))
        .await
        .unwrap();

    assert_eq!(provider.id(), plain.id());
}

#[sqlx::test]

async fn album_ladder_prefers_identifiers_then_normalised_text(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let artist = credit("Daft Punk", None);

    let first = repository
        .resolve_or_create_album(&album("Discovery").with_album_artist(artist.clone()))
        .await
        .unwrap();
    let same_text = repository
        .resolve_or_create_album(&album("  discovery ").with_album_artist(artist))
        .await
        .unwrap();
    assert_eq!(first.id(), same_text.id());

    let by_group = repository
        .resolve_or_create_album(
            &album("Totally Different")
                .with_release_group_id(Some("rg-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let same_group = repository
        .resolve_or_create_album(
            &album("Also Different")
                .with_release_group_id(Some("rg-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        by_group.id(),
        same_group.id(),
        "the group id wins over text"
    );
}

#[sqlx::test]

async fn an_identifier_attaches_to_a_text_only_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let plain = repository
        .resolve_or_create_album(&album("Discovery"))
        .await
        .unwrap();
    assert_eq!(plain.musicbrainz_release_group_id(), None);

    let discovered = repository
        .resolve_or_create_album(
            &album("Discovery")
                .with_release_group_id(Some("rg-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(discovered.id(), plain.id());
    assert_eq!(discovered.musicbrainz_release_group_id(), Some("rg-1"));
}

#[sqlx::test]

async fn two_identifiers_are_two_albums_even_when_the_text_agrees(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let original = repository
        .resolve_or_create_album(
            &album("Discovery")
                .with_release_group_id(Some("rg-original".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let remaster = repository
        .resolve_or_create_album(
            &album("Discovery")
                .with_release_group_id(Some("rg-remaster".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(original.id(), remaster.id());
}

#[sqlx::test]

async fn concurrent_album_resolution_creates_one_row(pool: PgPool) {
    const RESOLUTIONS: usize = 8;

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let barrier = Arc::new(Barrier::new(RESOLUTIONS));
    let mut tasks = Vec::with_capacity(RESOLUTIONS);

    for _ in 0..RESOLUTIONS {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);

        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .resolve_or_create_album(
                    &album("Discovery").with_album_artist(credit("Daft Punk", None)),
                )
                .await
        }));
    }

    let mut album_ids = Vec::with_capacity(RESOLUTIONS);
    for task in tasks {
        album_ids.push(task.await.unwrap().unwrap().id().clone());
    }

    assert!(album_ids.iter().all(|id| id == &album_ids[0]));

    let albums: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM albums")
        .fetch_one(&pool)
        .await
        .unwrap();
    let artists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(albums, 1);
    assert_eq!(artists, 1);
}

#[sqlx::test]

async fn album_unique_indexes_refuse_a_second_row(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    repository
        .resolve_or_create_album(&album("Discovery").with_album_artist(credit("Daft Punk", None)))
        .await
        .unwrap();

    let duplicate_text =
        sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, $2, $3)")
            .bind(Uuid::now_v7())
            .bind("Discovery")
            .bind(artist.id().as_uuid())
            .execute(&pool)
            .await;
    assert!(duplicate_text.is_err(), "the text index must refuse it");

    repository
        .resolve_or_create_album(
            &album("Remaster")
                .with_release_group_id(Some("rg-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    let duplicate_group = sqlx::query(
        "INSERT INTO albums (id, title, musicbrainz_release_group_id) VALUES ($1, $2, $3)",
    )
    .bind(Uuid::now_v7())
    .bind("Other")
    .bind("rg-1")
    .execute(&pool)
    .await;
    assert!(duplicate_group.is_err(), "the group index must refuse it");
}

#[sqlx::test]

async fn album_sources_and_membership_round_trip(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let album = repository
        .resolve_or_create_album(&album("Discovery"))
        .await
        .unwrap();
    let track_source = source("local-track");
    let track = repository
        .get_or_create(track_source.clone(), metadata("Song", &["Artist"], None))
        .await
        .unwrap();
    let album_source = source("youtube_music");
    let membership = AlbumTrack::new(Some(1), Some(3), 2).unwrap();

    repository
        .set_album_source(album.id(), &album_source)
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &album_source)
        .await
        .unwrap();
    assert_eq!(
        repository.find_album_sources(album.id()).await.unwrap(),
        vec![album_source]
    );

    repository
        .set_album_track(album.id(), track.id(), &membership)
        .await
        .unwrap();
    let entries = repository
        .find_album_entries(album.id(), None)
        .await
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].track().id(), track.id());
    assert_eq!(entries[0].membership(), &membership);
    assert_eq!(
        entries[0].sources(),
        std::slice::from_ref(&track_source),
        "an album entry carries the track's sources so a facade can label it"
    );
    assert_eq!(
        repository
            .find_album_for_track(track.id())
            .await
            .unwrap()
            .expect("the track is filed under the album")
            .id(),
        album.id()
    );
}

#[sqlx::test]

async fn provider_album_and_artist_identity_round_trip(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let album_source = source("MPREb_album");
    let artist_source = source("UC_artist");
    let credit = ArtistCredit::new("Olivia Rodrigo".to_owned(), None)
        .unwrap()
        .with_provider(Some(artist_source.clone()));
    let album_metadata = AlbumMetadata::new("SOUR".to_owned())
        .unwrap()
        .with_provider(Some(album_source.clone()));

    let album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &album_source)
        .await
        .unwrap();
    let artist = repository.resolve_or_create_artist(&credit).await.unwrap();
    repository
        .set_artist_source(artist.id(), &artist_source)
        .await
        .unwrap();

    assert_eq!(
        repository.find_album_sources(album.id()).await.unwrap(),
        vec![album_source],
        "the album's own provider identity is stored"
    );
    assert_eq!(
        repository.find_artist_sources(artist.id()).await.unwrap(),
        vec![artist_source],
        "the artist's provider identity is stored"
    );
}

#[sqlx::test]

async fn browse_reads_order_and_count_albums_and_artists(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let artist = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    let discovery = repository
        .resolve_or_create_album(&album("Discovery").with_album_artist(credit("Daft Punk", None)))
        .await
        .unwrap();
    let other = repository
        .resolve_or_create_album(&album("Homework").with_album_artist(credit("Daft Punk", None)))
        .await
        .unwrap();

    let third = repository
        .get_or_create(
            local_source("track-c.flac"),
            metadata("Third", &["Daft Punk"], Some(3_000)),
        )
        .await
        .unwrap();
    let first = repository
        .get_or_create(
            local_source("track-a.flac"),
            metadata("First", &["Daft Punk"], Some(1_000)),
        )
        .await
        .unwrap();
    let later = repository
        .get_or_create(
            local_source("track-b.flac"),
            metadata("Later", &["Daft Punk"], Some(2_000)),
        )
        .await
        .unwrap();

    repository
        .set_album_track(
            discovery.id(),
            third.id(),
            &AlbumTrack::new(Some(1), Some(3), 2).unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            discovery.id(),
            first.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            discovery.id(),
            later.id(),
            &AlbumTrack::new(None, None, 0).unwrap(),
        )
        .await
        .unwrap();

    let titles = repository
        .find_album_entries(discovery.id(), None)
        .await
        .unwrap()
        .iter()
        .map(|entry| entry.track().title().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        ["First", "Third", "Later"],
        "numbered tracks come first in track order, unnumbered last"
    );

    repository
        .set_album_track(
            other.id(),
            third.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .find_album_for_track(third.id())
            .await
            .unwrap()
            .expect("re-filing moves the track")
            .id(),
        other.id()
    );
    assert_eq!(
        repository
            .find_album_entries(discovery.id(), None)
            .await
            .unwrap()
            .len(),
        2,
        "the previous album no longer holds the track"
    );

    repository
        .set_album_track(
            discovery.id(),
            third.id(),
            &AlbumTrack::new(Some(1), Some(3), 2).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .find_album_for_track(third.id())
            .await
            .unwrap()
            .expect("re-filing back moves it again")
            .id(),
        discovery.id()
    );

    let summaries = repository
        .list_albums(0, 10, AlbumOrder::Name, AlbumListFilter::default(), None)
        .await
        .unwrap();
    let names = summaries
        .iter()
        .map(|summary| summary.album().title().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["Discovery"],
        "Homework lost its only track, so it is not browsable"
    );
    let listed = summaries
        .iter()
        .find(|summary| summary.album().id() == discovery.id())
        .expect("the album is listed");
    assert_eq!(listed.album_artist().unwrap().id(), artist.id());
    assert_eq!(listed.track_count(), 3);
    assert_eq!(listed.duration_ms(), 6_000);

    assert_eq!(
        repository
            .find_albums_for_artist(artist.id(), None)
            .await
            .unwrap()
            .len(),
        2
    );

    let artists = repository
        .list_artists(0, 10, ArtistSort::Name, &ArtistListFilter::default(), None)
        .await
        .unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].artist().id(), artist.id());
    assert_eq!(artists[0].album_count(), 2);
}

#[sqlx::test]

async fn browse_shows_only_engaged_albums_and_artists(pool: PgPool) {
    let user = Uuid::now_v7();
    insert_user(&pool, user, "engaged-listener").await;
    let repository = PostgresTrackRepository::new(pool.clone());

    let owned_album = repository
        .resolve_or_create_album(&album("Owned").with_album_artist(credit("Owner", None)))
        .await
        .unwrap();
    let played_album = repository
        .resolve_or_create_album(&album("Played").with_album_artist(credit("Player", None)))
        .await
        .unwrap();
    let untouched_album = repository
        .resolve_or_create_album(&album("Untouched").with_album_artist(credit("Stranger", None)))
        .await
        .unwrap();

    let owned_track = repository
        .get_or_create(
            local_source("owned.flac"),
            metadata("Owned Song", &["Owner"], Some(1_000)),
        )
        .await
        .unwrap();
    let played_track = repository
        .get_or_create(
            source("played-video"),
            metadata("Played Song", &["Player"], Some(2_000)),
        )
        .await
        .unwrap();
    let untouched_track = repository
        .get_or_create(
            source("untouched-video"),
            metadata("Untouched Song", &["Stranger"], Some(3_000)),
        )
        .await
        .unwrap();

    for (album_row, track) in [
        (&owned_album, &owned_track),
        (&played_album, &played_track),
        (&untouched_album, &untouched_track),
    ] {
        repository
            .set_album_track(
                album_row.id(),
                track.id(),
                &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
            )
            .await
            .unwrap();
    }

    seed_play(&pool, user, played_track.id(), chrono::Utc::now()).await;

    let listed = |order: AlbumOrder| {
        let repository = &repository;
        async move {
            repository
                .list_albums(0, 25, order, AlbumListFilter::default(), Some(user))
                .await
                .unwrap()
                .into_iter()
                .map(|summary| summary.album().title().to_owned())
                .collect::<Vec<_>>()
        }
    };

    for order in [
        AlbumOrder::Name,
        AlbumOrder::AlbumArtist,
        AlbumOrder::Random,
        AlbumOrder::MaxYear,
        AlbumOrder::MaxYear,
        AlbumOrder::StarredAt,
        AlbumOrder::Rating,
        AlbumOrder::PlayDate,
        AlbumOrder::PlayCount,
    ] {
        let names = listed(order).await;
        assert!(
            !names.iter().any(|name| name == "Untouched"),
            "{order:?} must not show an album nothing engaged with, got {names:?}"
        );
    }

    let played = listed(AlbumOrder::PlayDate).await;
    assert!(
        played.iter().any(|name| name == "Played"),
        "a played provider album belongs in recently played, got {played:?}"
    );

    let newest = repository
        .list_albums(
            0,
            25,
            AlbumOrder::RecentlyAdded,
            AlbumListFilter::new().with_scope(AlbumListScope::Owned),
            Some(user),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.album().title().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !newest.iter().any(|name| name == "Played"),
        "a played provider album is not recently added, got {newest:?}"
    );
    assert!(
        newest.iter().any(|name| name == "Owned"),
        "an owned album is recently added, got {newest:?}"
    );

    let artists = repository
        .list_artists(
            0,
            25,
            ArtistSort::Name,
            &ArtistListFilter::default(),
            Some(user),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.artist().name().to_owned())
        .collect::<Vec<_>>();
    assert!(
        !artists.iter().any(|name| name == "Stranger"),
        "an artist nothing engaged with is not listed, got {artists:?}"
    );
    assert!(
        artists.iter().any(|name| name == "Owner"),
        "got {artists:?}"
    );
    assert!(
        artists.iter().any(|name| name == "Player"),
        "got {artists:?}"
    );

    assert_eq!(
        repository
            .count_albums(Some(user), AlbumListFilter::default())
            .await
            .unwrap(),
        2,
        "the count agrees with the list"
    );
}
#[sqlx::test]

async fn recording_identifiers_round_trip_on_the_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let stored = repository
        .get_or_create(
            source("local-recording"),
            metadata("Song", &["Artist"], None)
                .with_recording_id(Some("mb-recording".to_owned()))
                .unwrap()
                .with_isrc(Some("ISRC-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();

    let found = repository
        .find_track(stored.id())
        .await
        .unwrap()
        .expect("stored track is found");
    assert_eq!(found.musicbrainz_recording_id(), Some("mb-recording"));
    assert_eq!(found.isrc(), Some("ISRC-1"));
}

#[sqlx::test]

async fn performer_credits_link_to_artist_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = TrackMetadata::new(
        "Song".to_owned(),
        vec![
            ArtistCredit::new("Daft Punk".to_owned(), Some("mb-artist-1".to_owned())).unwrap(),
            ArtistCredit::new("Pharrell".to_owned(), None).unwrap(),
        ],
        None,
    )
    .unwrap();
    let stored = repository
        .get_or_create(source("linked-credits"), metadata)
        .await
        .unwrap();

    let rows: Vec<(i32, String, Option<Uuid>)> = sqlx::query_as(
        r#"SELECT position, name, artist_id
        FROM track_artist_credits
        WHERE track_id = $1
        ORDER BY position
        "#,
    )
    .bind(stored.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1, "Daft Punk");
    assert_eq!(rows[1].1, "Pharrell");
    let daft_punk = rows[0].2.expect("an id-bearing credit links");
    assert!(rows[1].2.is_some(), "a name-only credit still links");

    let identifier: Option<String> =
        sqlx::query_scalar("SELECT musicbrainz_artist_id FROM artists WHERE id = $1")
            .bind(daft_punk)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identifier.as_deref(), Some("mb-artist-1"));

    let found = repository
        .find_track(stored.id())
        .await
        .unwrap()
        .expect("stored track is found");
    assert_eq!(found.artists().len(), 2);
    assert_eq!(found.artists()[0].name(), "Daft Punk");
    assert_eq!(
        found.artists()[0].musicbrainz_artist_id(),
        Some("mb-artist-1")
    );
    assert_eq!(found.artists()[1].musicbrainz_artist_id(), None);

    let second = TrackMetadata::new(
        "Other".to_owned(),
        vec![ArtistCredit::new("Pharrell".to_owned(), None).unwrap()],
        None,
    )
    .unwrap();
    repository
        .get_or_create(source("linked-credits-2"), second)
        .await
        .unwrap();

    let pharrell: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM artists WHERE name = 'Pharrell'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pharrell, 1, "the same name-only artist is reused");
}

#[sqlx::test]

async fn search_finds_a_title_across_typographic_variants(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let track = repository
        .get_or_create(
            local_source("path-a"),
            metadata(
                "It\u{2019}s Just Forever",
                &["Cage the Elephant"],
                Some(1_000),
            ),
        )
        .await
        .unwrap();

    for query in ["it's just forever", "it\u{2019}s just forever", "IT'S JUST"] {
        let hits = repository.search_tracks(query, 10, None).await.unwrap();
        assert_eq!(hits.len(), 1, "query {query:?} found nothing");
        assert_eq!(hits[0].track().id(), track.id());
    }
}

#[sqlx::test]

async fn search_ranks_match_quality_before_ownership(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let exact = repository
        .get_or_create(
            local_source("a"),
            metadata("Zebra", &["Daft Punk"], Some(1_000)),
        )
        .await
        .unwrap();
    let prefix = repository
        .get_or_create(
            local_source("b"),
            metadata("Zebra Crossing", &["Other"], Some(1_000)),
        )
        .await
        .unwrap();
    let word = repository
        .get_or_create(source("c"), metadata("The Zebra", &["Other"], Some(1_000)))
        .await
        .unwrap();

    let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
    let ordered = hits
        .iter()
        .map(|hit| (hit.track().id().clone(), hit.match_quality()))
        .collect::<Vec<_>>();

    assert_eq!(
        ordered,
        vec![
            (exact.id().clone(), Some(MatchQuality::Exact)),
            (prefix.id().clone(), Some(MatchQuality::Prefix)),
            (word.id().clone(), Some(MatchQuality::WordBoundary)),
        ]
    );
}

#[sqlx::test]

async fn search_does_not_match_inside_a_word(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    repository
        .get_or_create(
            local_source("a"),
            metadata("all-american bitch", &["Olivia Rodrigo"], Some(1_000)),
        )
        .await
        .unwrap();

    assert!(
        repository
            .search_tracks("me", 10, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repository
            .search_tracks("american", 10, None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repository
            .search_tracks("bitch", 10, None)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test]

async fn search_puts_an_artist_only_match_last(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let titled = repository
        .get_or_create(
            local_source("a"),
            metadata("The End", &["Other"], Some(1_000)),
        )
        .await
        .unwrap();
    let artist_only = repository
        .get_or_create(
            source("c"),
            metadata("2024", &["Cage the Elephant"], Some(1_000)),
        )
        .await
        .unwrap();

    let hits = repository.search_tracks("the", 10, None).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].track().id(), titled.id());
    assert_eq!(hits[0].match_quality(), Some(MatchQuality::Prefix));
    assert_eq!(hits[1].track().id(), artist_only.id());
    assert_eq!(hits[1].match_quality(), Some(MatchQuality::ArtistOnly));
}

#[sqlx::test]

async fn search_artists_collapses_rows_sharing_a_name(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let identified = repository
        .resolve_or_create_artist(&credit("Cage the Elephant", Some("mb-1")))
        .await
        .unwrap();
    repository
        .resolve_or_create_artist(&credit("Cage The Elephant", None))
        .await
        .unwrap();

    let artists = repository
        .search_artists("cage", 10, None, &ArtistListFilter::default())
        .await
        .unwrap();
    assert_eq!(artists.len(), 1);
    assert_eq!(artists[0].artist().id(), identified.id());
    assert!(artists[0].artist().musicbrainz_artist_id().is_some());
}

#[sqlx::test]

async fn candidate_tracks_resolves_a_page_of_titles(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let first = repository
        .get_or_create(
            local_source("a"),
            metadata(
                "It\u{2019}s Just Forever",
                &["Cage the Elephant"],
                Some(1_000),
            ),
        )
        .await
        .unwrap();
    repository
        .get_or_create(
            local_source("b"),
            metadata("Other", &["Other"], Some(1_000)),
        )
        .await
        .unwrap();

    let hits = repository
        .candidate_tracks(&["It's Just Forever".to_owned(), "missing".to_owned()], 25)
        .await
        .unwrap();

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track().id(), first.id());
    assert!(hits[0].match_quality().is_none());
}

fn facts(path: &str, size_bytes: u64, suffix: &str) -> SourceFileFacts {
    SourceFileFacts {
        path: Some(path.to_owned()),
        size_bytes,
        modified_at: None,
        missing_at: None,
        content_type: None,
        suffix: Some(suffix.to_owned()),
        container: None,
        codec: None,
        bitrate_kbps: None,
        sample_rate_hz: None,
        bit_depth: None,
        channels: None,
        facts_version: 1,
        acoustid_fingerprint: None,
    }
}

fn identified(title: &str, recording_id: &str) -> TrackMetadata {
    metadata(title, &["Artist"], Some(1_000))
        .with_recording_id(Some(recording_id.to_owned()))
        .unwrap()
}

#[sqlx::test]

async fn two_files_of_one_recording_share_a_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let first = local_source("album-a/01 song.flac");
    let second = local_source("album-b/01 song.flac");

    let first_track = repository
        .get_or_create(first.clone(), identified("Song", "rec-1"))
        .await
        .unwrap();
    let second_track = repository
        .get_or_create(second.clone(), identified("Song", "rec-1"))
        .await
        .unwrap();

    assert_eq!(first_track.id(), second_track.id());

    let sources = repository.find_sources(first_track.id()).await.unwrap();
    assert_eq!(sources.len(), 2, "both files are sources of the one track");
    assert!(sources.contains(&first));
    assert!(sources.contains(&second));
}

#[sqlx::test]

async fn a_moved_file_keeps_its_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let moved = local_source("new/01 song.flac");

    let before = repository
        .get_or_create(
            local_source("old/01 song.flac"),
            identified("Song", "rec-1"),
        )
        .await
        .unwrap();
    let after = repository
        .get_or_create(moved.clone(), identified("Song", "rec-1"))
        .await
        .unwrap();

    assert_eq!(before.id(), after.id());
    assert!(
        repository
            .find_sources(before.id())
            .await
            .unwrap()
            .contains(&moved)
    );
}

#[sqlx::test]
async fn a_provider_source_attaches_to_one_matching_library_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let local = repository
        .get_or_create(
            local_source("glue-song.flac"),
            metadata("Glue Song", &["beabadoobee"], Some(180_000)),
        )
        .await
        .unwrap();

    let provider = repository
        .get_or_create_provider(
            source("glue-song-video"),
            metadata("Glue Song", &["beabadoobee"], Some(181_000)),
        )
        .await
        .unwrap();

    assert_eq!(provider.id(), local.id());
    assert_eq!(repository.find_sources(local.id()).await.unwrap().len(), 2);
}

#[sqlx::test]
async fn provider_matching_rejects_mismatched_secondary_artists(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let library = repository
        .get_or_create(
            local_source("secondary-credit.flac"),
            metadata("Shared Song", &["Primary", "Library Guest"], Some(180_000)),
        )
        .await
        .unwrap();

    let provider = repository
        .get_or_create_provider(
            source("secondary-credit-video"),
            metadata(
                "Shared Song",
                &["Primary", "Different Guest"],
                Some(180_000),
            ),
        )
        .await
        .unwrap();

    assert_ne!(provider.id(), library.id());
}

#[sqlx::test]
async fn provider_feature_superset_uses_the_stricter_duration_tolerance(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let library = repository
        .get_or_create(
            local_source("feature-superset.flac"),
            metadata("Shared Song", &["Primary"], Some(180_000)),
        )
        .await
        .unwrap();

    let provider = repository
        .get_or_create_provider(
            source("feature-superset-video"),
            metadata("Shared Song", &["Primary", "Guest"], Some(181_500)),
        )
        .await
        .unwrap();

    assert_ne!(provider.id(), library.id());
}

#[sqlx::test]
async fn a_provider_source_outside_duration_tolerance_stays_separate(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let local = repository
        .get_or_create(
            local_source("glue-song.flac"),
            metadata("Glue Song", &["beabadoobee"], Some(180_000)),
        )
        .await
        .unwrap();

    let provider = repository
        .get_or_create_provider(
            source("glue-song-video"),
            metadata("Glue Song", &["beabadoobee"], Some(182_001)),
        )
        .await
        .unwrap();

    assert_ne!(provider.id(), local.id());
}

#[sqlx::test]
async fn one_matching_track_is_selected_among_similar_library_tracks(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let matching = repository
        .get_or_create(
            local_source("matching-similar.flac"),
            metadata("Shared Song", &["Matching Artist"], Some(180_000)),
        )
        .await
        .unwrap();
    repository
        .get_or_create(
            local_source("other-similar.flac"),
            metadata("Shared Song", &["Other Artist"], Some(180_000)),
        )
        .await
        .unwrap();

    let provider = repository
        .get_or_create_provider(
            source("unique-match-video"),
            metadata("Shared Song", &["Matching Artist"], Some(180_000)),
        )
        .await
        .unwrap();

    assert_eq!(provider.id(), matching.id());
}

#[sqlx::test]
async fn several_library_matches_do_not_attach_a_provider_source(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    for path in ["glue-song-a.flac", "glue-song-b.flac"] {
        repository
            .get_or_create(
                local_source(path),
                metadata("Glue Song", &["beabadoobee"], Some(180_000)),
            )
            .await
            .unwrap();
    }

    let provider = repository
        .get_or_create_provider(
            source("glue-song-video"),
            metadata("Glue Song", &["beabadoobee"], Some(180_000)),
        )
        .await
        .unwrap();

    assert_eq!(
        repository.find_sources(provider.id()).await.unwrap().len(),
        1
    );
    let library_tracks: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT track_id) FROM track_sources WHERE provider_id = 'local'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(library_tracks, 2);
}

#[sqlx::test]

async fn an_unidentified_file_keeps_its_own_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let first = repository
        .get_or_create(
            local_source("a.flac"),
            metadata("Song", &["Artist"], Some(1_000)),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            local_source("b.flac"),
            metadata("Song", &["Artist"], Some(1_000)),
        )
        .await
        .unwrap();

    assert_ne!(first.id(), second.id());
}

#[sqlx::test]

async fn gaining_an_identifier_already_held_moves_the_source(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let owned = local_source("album-a/01 song.flac");

    let identified_track = repository
        .get_or_create(
            local_source("album-b/01 song.flac"),
            identified("Song", "rec-1"),
        )
        .await
        .unwrap();
    let plain_track = repository
        .get_or_create(owned.clone(), metadata("Song", &["Artist"], Some(1_000)))
        .await
        .unwrap();
    assert_ne!(plain_track.id(), identified_track.id());

    let merged = repository
        .get_or_create(owned.clone(), identified("Song", "rec-1"))
        .await
        .unwrap();

    assert_eq!(merged.id(), identified_track.id());
    assert!(
        repository
            .find_sources(merged.id())
            .await
            .unwrap()
            .contains(&owned)
    );
    assert!(
        repository
            .find_track(plain_track.id())
            .await
            .unwrap()
            .is_none(),
        "the vacated track had no other source"
    );
}

#[sqlx::test]

async fn one_recording_id_admits_one_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    repository
        .get_or_create(local_source("a.flac"), identified("Song", "rec-1"))
        .await
        .unwrap();

    let refused = sqlx::query(
        "INSERT INTO tracks (id, title, musicbrainz_recording_id) VALUES ($1, 'Other', 'rec-1')",
    )
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await;

    assert!(refused.is_err(), "a second track may not claim rec-1");
}

#[sqlx::test]

async fn a_rejected_file_leaves_no_source_behind(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let source = local_source("bad.flac");

    let refused = repository
        .index_source(
            source.clone(),
            metadata("Song", &["Artist"], Some(1_000)),
            &facts("bad.flac", 10, &"x".repeat(40)),
        )
        .await;
    assert!(refused.is_err(), "the suffix exceeds its bound");

    assert!(
        repository
            .find_source_file_facts(&source)
            .await
            .unwrap()
            .is_none(),
        "a rejected write leaves no source row"
    );
    assert!(
        repository
            .search_tracks("song", 10, None)
            .await
            .unwrap()
            .is_empty(),
        "and no track"
    );

    repository
        .index_source(
            source.clone(),
            metadata("Song", &["Artist"], Some(1_000)),
            &facts("bad.flac", 10, "flac"),
        )
        .await
        .unwrap();

    assert!(
        repository
            .find_source_file_facts(&source)
            .await
            .unwrap()
            .is_some(),
        "the same file indexes once its facts are valid"
    );
}

#[sqlx::test]

async fn duplicate_candidates_report_local_copies(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let first = repository
        .index_source(
            local_source("a/song.flac"),
            identified("Song", "rec-1"),
            &facts("a/song.flac", 900, "flac"),
        )
        .await
        .unwrap();
    repository
        .index_source(
            local_source("b/song.mp3"),
            identified("Song", "rec-1"),
            &facts("b/song.mp3", 100, "mp3"),
        )
        .await
        .unwrap();
    repository
        .index_source(
            local_source("c/other.flac"),
            identified("Other", "rec-2"),
            &facts("c/other.flac", 500, "flac"),
        )
        .await
        .unwrap();

    let candidates = repository.duplicate_candidates(10).await.unwrap();
    assert_eq!(
        candidates.len(),
        1,
        "only the track holding two files is reported"
    );

    let candidate = &candidates[0];
    assert_eq!(candidate.track_id(), first.id());
    assert_eq!(candidate.title(), "Song");
    assert_eq!(candidate.sources().len(), 2);
    assert_eq!(candidate.wasted_bytes(), 100);

    let (keep, reason) = candidate.keep().unwrap();
    assert_eq!(keep.path(), "a/song.flac");
    assert_eq!(reason, KeepReason::LosslessOverLossy);
}

#[sqlx::test]

async fn split_artists_report_rows_sharing_a_name(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let identified = repository
        .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
        .await
        .unwrap();
    let plain_id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, $2)")
        .bind(plain_id)
        .bind("Daft Punk")
        .execute(&pool)
        .await
        .unwrap();
    repository
        .resolve_or_create_artist(&credit("Aphex Twin", None))
        .await
        .unwrap();

    let split = repository.split_artists(10).await.unwrap();
    assert_eq!(split.len(), 1, "only the shared name is reported");

    let artist = &split[0];
    assert_eq!(artist.name(), "daft punk");
    assert_eq!(artist.rows().len(), 2);
    assert!(artist.has_identifier());

    let with_id = artist
        .rows()
        .iter()
        .find(|row| row.is_identified())
        .expect("the id-bearing row is reported");
    assert_eq!(with_id.artist_id(), identified.id());

    let without_id = artist
        .rows()
        .iter()
        .find(|row| !row.is_identified())
        .expect("the name-only row is reported");
    assert_eq!(without_id.artist_id().as_uuid(), &plain_id);
}

#[sqlx::test]

async fn search_ranks_local_sources_first_and_matches_credits(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);

    let owned = repository
        .get_or_create(
            local_source("path-a"),
            metadata("Zebra", &["Daft Punk"], Some(1_000)),
        )
        .await
        .unwrap();
    let remote = repository
        .get_or_create(
            source("vid-1"),
            metadata("Apple", &["Daft Punk"], Some(1_000)),
        )
        .await
        .unwrap();

    let hits = repository
        .search_tracks("daft punk", 10, None)
        .await
        .unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits[0].track().id(),
        owned.id(),
        "the local track outranks the remote one"
    );
    assert_eq!(hits[1].track().id(), remote.id());

    let by_title = repository.search_tracks("zeb", 10, None).await.unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].track().id(), owned.id());
    assert!(
        repository
            .search_tracks("nothing", 10, None)
            .await
            .unwrap()
            .is_empty()
    );

    let album_metadata = album("Discovery").with_album_artist(credit("Daft Punk", None));
    let album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            owned.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();

    let albums = repository.search_albums("disc", 10, None).await.unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].album().id(), album.id());
    assert_eq!(albums[0].track_count(), 1);

    let artists = repository
        .search_artists("daft", 10, None, &ArtistListFilter::default())
        .await
        .unwrap();
    assert!(
        artists
            .iter()
            .any(|artist| artist.artist().name() == "Daft Punk")
    );
    assert!(
        repository
            .search_artists("nope", 10, None, &ArtistListFilter::default())
            .await
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        repository
            .count_albums(None, AlbumListFilter::default())
            .await
            .unwrap(),
        1
    );
    assert!(
        repository
            .count_artists(None, &ArtistListFilter::default())
            .await
            .unwrap()
            >= 1
    );
}

#[sqlx::test]

async fn per_user_state_and_genres_round_trip(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = Uuid::now_v7();
    let other = Uuid::now_v7();
    insert_user(&pool, user, "state-user").await;
    insert_user(&pool, other, "other-user").await;

    let album = repository
        .resolve_or_create_album(
            &album("Discovery")
                .with_album_artist(credit("Daft Punk", None))
                .with_year(Some(2001))
                .unwrap(),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            local_source("path-a"),
            metadata("First", &["Daft Punk"], Some(1_000)).with_genres(vec!["Rock".to_owned()]),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            track.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();
    let artist = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();

    repository
        .set_starred(user, StateTarget::Track(track.id()), true)
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Album(album.id()), true)
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Artist(artist.id()), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(track.id()), Some(4))
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Album(album.id()), Some(5))
        .await
        .unwrap();
    seed_play(&pool, user, track.id(), chrono::Utc::now()).await;
    seed_play(&pool, user, track.id(), chrono::Utc::now()).await;

    let state = repository.find_track_state(track.id(), user).await.unwrap();
    assert!(state.is_starred());
    assert_eq!(state.rating(), Some(4));
    assert_eq!(state.play_count(), 2);

    let other_state = repository
        .find_track_state(track.id(), other)
        .await
        .unwrap();
    assert!(!other_state.is_starred());
    assert_eq!(other_state.play_count(), 0);

    let starred = repository.starred(user).await.unwrap();
    assert_eq!(starred.tracks().len(), 1);
    assert_eq!(starred.albums().len(), 1);
    assert_eq!(starred.artists().len(), 1);

    let genres = repository.list_genres().await.unwrap();
    assert_eq!(genres.len(), 1);
    assert_eq!(genres[0].genre(), "Rock");
    assert_eq!(genres[0].song_count(), 1);
    assert_eq!(genres[0].album_count(), 1);

    let frequent = repository
        .list_albums(
            0,
            10,
            AlbumOrder::PlayCount,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(frequent[0].album().id(), album.id());
    assert_eq!(frequent[0].state().play_count(), 2);

    let highest = repository
        .list_albums(
            0,
            10,
            AlbumOrder::Rating,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(highest[0].album().id(), album.id());

    let starred_albums = repository
        .list_albums(
            0,
            10,
            AlbumOrder::StarredAt,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(starred_albums[0].album().id(), album.id());

    let by_genre = repository
        .list_albums(
            0,
            10,
            AlbumOrder::Name,
            AlbumListFilter::new().with_genre(Some(genre_set(&["rock"]))),
            None,
        )
        .await
        .unwrap();
    assert_eq!(by_genre.len(), 1);

    let by_year = repository
        .list_albums(
            0,
            10,
            AlbumOrder::MaxYear,
            AlbumListFilter::new().with_year(Some(year_range(1900, 2100))),
            None,
        )
        .await
        .unwrap();
    assert_eq!(by_year.len(), 1);

    repository
        .set_rating(user, StateTarget::Track(track.id()), Some(0))
        .await
        .unwrap();
    assert_eq!(
        repository
            .find_track_state(track.id(), user)
            .await
            .unwrap()
            .rating(),
        None
    );
}

#[sqlx::test]

async fn dedup_candidates_return_local_matches_and_strip_qualifiers(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let plain = repository
        .get_or_create(local_source("a"), metadata("Song", &["A"], Some(240_000)))
        .await
        .unwrap();
    let live = repository
        .get_or_create(
            local_source("b"),
            metadata("Song (Live)", &["A"], Some(240_000)),
        )
        .await
        .unwrap();
    repository
        .get_or_create(source("remote"), metadata("Song", &["A"], Some(240_000)))
        .await
        .unwrap();

    let hits = repository
        .dedup_candidates(&["song".to_owned()], 25)
        .await
        .unwrap();
    let ids = hits
        .iter()
        .map(|hit| hit.track().id().clone())
        .collect::<Vec<_>>();

    assert!(ids.contains(plain.id()));
    assert!(
        ids.contains(live.id()),
        "the trigram branch reaches a qualifier"
    );
    assert_eq!(ids.len(), 2, "the provider track is not a dedup candidate");
}

#[sqlx::test]
async fn provider_matching_rejects_candidate_overflow_for_attach_and_prune(pool: PgPool) {
    let target_artist = Uuid::now_v7();
    let other_artist = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO artists (id, name) VALUES ($1, 'Overflow Target'), ($2, 'Overflow Other')",
    )
    .bind(target_artist)
    .bind(other_artist)
    .execute(&pool)
    .await
    .unwrap();
    let track_ids = (1..=502).map(Uuid::from_u128).collect::<Vec<_>>();
    let artist_ids = (1..=502)
        .map(|index| {
            if index == 1 || index == 502 {
                target_artist
            } else {
                other_artist
            }
        })
        .collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO tracks (id, title, duration_ms) SELECT id, 'Overflow Song', 180000 FROM unnest($1::UUID[]) AS tracks(id)",
    )
    .bind(&track_ids)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_artist_credits (track_id, position, name, artist_id) SELECT item.track_id, 0, artist.name, artist.id FROM unnest($1::UUID[], $2::UUID[]) AS item(track_id, artist_id) JOIN artists AS artist ON artist.id = item.artist_id",
    )
    .bind(&track_ids)
    .bind(&artist_ids)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id) SELECT id, id, 'local', 'named:/music', 'overflow-' || id::TEXT FROM unnest($1::UUID[]) AS tracks(id)",
    )
    .bind(&track_ids)
    .execute(&pool)
    .await
    .unwrap();
    let repository = PostgresTrackRepository::new(pool.clone());
    let provider = repository
        .get_or_create_provider(
            source("overflow-provider"),
            metadata("Overflow Song", &["Overflow Target"], Some(180_000)),
        )
        .await
        .unwrap();

    assert_ne!(provider.id().as_uuid(), &track_ids[0]);
    let report = repository.provider_duplicates().await.unwrap();
    assert!(
        !report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == *provider.id().as_uuid())
    );
}

#[sqlx::test]
async fn dedup_candidates_hydrate_with_a_one_connection_pool(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    repository
        .get_or_create(
            local_source("single-connection.flac"),
            metadata("Single Connection", &["Artist"], Some(240_000)),
        )
        .await
        .unwrap();
    let single_connection_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let single_connection_repository = PostgresTrackRepository::new(single_connection_pool);

    let hits = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        single_connection_repository.dedup_candidates(&["Single Connection".to_owned()], 10),
    )
    .await
    .expect("candidate hydration completes")
    .unwrap();

    assert_eq!(hits.len(), 1);
}

#[sqlx::test]

async fn search_suppression_uses_postgres_candidates_and_keeps_saved_remote_identity(pool: PgPool) {
    use crate::{
        features::music_library::catalogue::{
            SearchLimit, SearchOffset, SearchQuery, SearchRequest, SearchService, SearchWindow,
        },
        platform::request::{RequestContext, RequestId},
    };
    let root = std::env::temp_dir().join(format!("whio-pg-dedup-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("song.flac"), b"media").unwrap();
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let local_source = SourceIdentity::new(
        ProviderId::new("local".into()).unwrap(),
        SourceScope::Named(root.display().to_string()),
        "song.flac".into(),
    )
    .unwrap();
    let local = repository
        .index_source(
            local_source,
            metadata("Song", &["Artist"], Some(240000)),
            &SourceFileFacts {
                path: Some("song.flac".into()),
                size_bytes: 5,
                modified_at: None,
                missing_at: None,
                content_type: None,
                suffix: Some("flac".into()),
                container: None,
                codec: None,
                bitrate_kbps: None,
                sample_rate_hz: None,
                bit_depth: None,
                channels: None,
                facts_version: 1,
                acoustid_fingerprint: None,
            },
        )
        .await
        .unwrap();
    let remote = repository
        .get_or_create(source("video"), metadata("Song", &["Artist"], Some(240000)))
        .await
        .unwrap();
    let user = Uuid::now_v7();
    let service = SearchService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    )
    .with_dedup_budget(std::time::Duration::from_secs(10));
    let search = SearchRequest::uniform(
        SearchQuery::new("Song".into()).unwrap(),
        SearchWindow::new(SearchLimit::new(25).unwrap(), SearchOffset::new(0).unwrap()),
    );
    let context = RequestContext::new(RequestId::new("pg-dedup".into()).unwrap());
    let outcome = service.search(&search, None, &context).await.unwrap();
    assert_eq!(outcome.tracks.len(), 1);
    assert_eq!(outcome.tracks[0].track().id(), local.id());
    assert!(repository.find_track(remote.id()).await.unwrap().is_some());
    std::fs::remove_dir_all(&root).unwrap();
    let outcome = service
        .search(&search, Some(&user), &context)
        .await
        .unwrap();
    assert!(
        outcome
            .tracks
            .iter()
            .any(|hit| hit.track().id() == remote.id())
    );
}
