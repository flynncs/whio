#![cfg(feature = "postgres-integration")]

use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::Barrier;
use uuid::Uuid;
use whio_core::tracks::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ArtworkRef, PostgresTrackRepository, ProviderId,
    SourceFileFacts, SourceIdentity, SourceScope, TrackId, TrackMetadata, TrackRepository,
    TrackRepositoryError,
};

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Named("test-installation".to_owned()),
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
async fn album_refreshes_within_a_provider_and_artwork_follows_its_source(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("metadata-source");

    let with_album = |album: &str| {
        metadata("Song", &["Artist"], None)
            .with_album(Some(album.to_owned()))
            .unwrap()
    };

    let track = repository
        .get_or_create(source.clone(), with_album("Provisional"))
        .await
        .unwrap();
    assert_eq!(track.album(), Some("Provisional"));

    let refreshed = repository
        .get_or_create(source.clone(), with_album("Authoritative"))
        .await
        .unwrap();
    assert_eq!(refreshed.id(), track.id());
    assert_eq!(refreshed.album(), Some("Authoritative"));

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
#[ignore = "requires PostgreSQL"]
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

fn credit(name: &str, identifier: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), identifier.map(str::to_owned)).unwrap()
}

fn album(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
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
    assert_ne!(
        plain.id(),
        attached.id(),
        "an id-bearing artist and a name-only artist coexist"
    );
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
async fn album_sources_and_membership_round_trip(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let album = repository
        .resolve_or_create_album(&album("Discovery"))
        .await
        .unwrap();
    let track = repository
        .get_or_create(source("local-track"), metadata("Song", &["Artist"], None))
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
    assert_eq!(
        repository.find_album_tracks(album.id()).await.unwrap(),
        vec![(track.id().clone(), membership)]
    );
}

#[sqlx::test]
#[ignore = "requires PostgreSQL"]
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
#[ignore = "requires PostgreSQL"]
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
