use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, StateTarget, TrackMetadata,
};
use crate::features::music_library::library_state::test_support::seed_play;
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, Retention, StateStore,
};
use sqlx::PgPool;
use uuid::Uuid;

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str, artist: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec![artist.to_owned()], Some(1_000)).unwrap()
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

async fn retention_reports_and_evicts_only_unengaged_provider_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = Uuid::now_v7();
    insert_user(&pool, user, "flynn").await;

    let owned = repository
        .get_or_create(local_source("owned.flac"), metadata("Owned", "Owner"))
        .await
        .unwrap();
    let orphan = repository
        .get_or_create(source("orphan"), metadata("Orphan", "Ghost"))
        .await
        .unwrap();
    let starred = repository
        .get_or_create(source("starred"), metadata("Starred", "Star"))
        .await
        .unwrap();
    let played = repository
        .get_or_create(source("played"), metadata("Played", "Play"))
        .await
        .unwrap();
    let listed = repository
        .get_or_create(source("listed"), metadata("Listed", "List"))
        .await
        .unwrap();

    let played_at = "2026-09-01T00:00:00Z".parse().unwrap();
    repository
        .set_starred(user, StateTarget::Track(starred.id()), true)
        .await
        .unwrap();
    seed_play(&pool, user, played.id(), played_at).await;

    let playlist_id = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'mix')")
        .bind(playlist_id)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_entries (playlist_id, position, track_id) VALUES ($1, 0, $2)",
    )
    .bind(playlist_id)
    .bind(listed.id().as_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Lost".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_lost"))
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), orphan.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let report = repository.retention_report().await.unwrap();
    assert_eq!(report.evictable_tracks, 1, "only the unengaged track");
    assert_eq!(report.evictable_albums, 1);
    assert_eq!(report.evictable_artists, 1, "only the ghost artist");
    assert_eq!(report.play_held_tracks, 1);
    assert_eq!(report.play_held_albums, 0);
    assert_eq!(report.play_held_artists, 1);
    assert_eq!(report.oldest_played_at, Some(played_at));
    assert_eq!(report.newest_played_at, Some(played_at));

    let evicted = repository.evict_unengaged().await.unwrap();
    assert_eq!(evicted.tracks, 1);
    assert_eq!(evicted.albums, 1);
    assert_eq!(evicted.artists, 1);

    assert!(repository.find_track(orphan.id()).await.unwrap().is_none());
    for kept in [owned.id(), starred.id(), played.id(), listed.id()] {
        assert!(
            repository.find_track(kept).await.unwrap().is_some(),
            "an engaged or owned track survives"
        );
    }

    let after = repository.retention_report().await.unwrap();
    assert_eq!(after.evictable_tracks, 0);
    assert_eq!(after.evictable_albums, 0);
    assert_eq!(after.evictable_artists, 0);
    assert_eq!(after.play_held_tracks, 1);
}

async fn matching_provider_track(
    repository: &PostgresTrackRepository,
    title: &str,
    artist: &str,
) -> (
    crate::features::music_library::domain::TrackId,
    crate::features::music_library::domain::TrackId,
) {
    let library = repository
        .get_or_create(
            local_source(&format!("{title}.flac")),
            metadata(title, artist),
        )
        .await
        .unwrap();
    let provider = repository
        .get_or_create(
            source(&format!("provider-{title}")),
            metadata(title, artist),
        )
        .await
        .unwrap();
    (library.id().clone(), provider.id().clone())
}

async fn seed_legacy_track(
    pool: &PgPool,
    track_id: Uuid,
    title: &str,
    artist_id: Uuid,
    artist_name: &str,
    source: (&str, &str, &str),
) {
    sqlx::query("INSERT INTO tracks (id, title, duration_ms) VALUES ($1, $2, 180000)")
        .bind(track_id)
        .bind(title)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_artist_credits (track_id, position, name, artist_id) VALUES ($1, 0, $2, $3)",
    )
    .bind(track_id)
    .bind(artist_name)
    .bind(artist_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(track_id)
    .bind(source.0)
    .bind(source.1)
    .bind(source.2)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn apply_removes_a_legacy_provider_artist_album_and_track_only(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let library_artist = Uuid::now_v7();
    let provider_artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, 'beabadoobee', 'bea-library')")
        .bind(library_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, 'beabadoobee')")
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'bea-provider')")
        .bind(Uuid::now_v7())
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    let library_track = Uuid::now_v7();
    let provider_track = Uuid::now_v7();
    seed_legacy_track(
        &pool,
        library_track,
        "Glue Song",
        library_artist,
        "beabadoobee",
        ("local", "named:/music", "glue.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        provider_track,
        "Glue Song",
        provider_artist,
        "beabadoobee",
        ("youtube_music", "global", "glue-video"),
    )
    .await;
    let library_album = Uuid::now_v7();
    let provider_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, 'Beatopia', $2, 'rg-beatopia')")
        .bind(library_album)
        .bind(library_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Beatopia', $2)")
        .bind(provider_album)
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'beatopia-provider')")
        .bind(Uuid::now_v7())
        .bind(provider_album)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, 0), ($3, $4, 0)",
    )
    .bind(library_album)
    .bind(library_track)
    .bind(provider_album)
    .bind(provider_track)
    .execute(&pool)
    .await
    .unwrap();

    let report = repository.delete_provider_duplicates().await.unwrap();

    assert_eq!(report.duplicates.len(), 3);
    assert!(
        report
            .duplicates
            .iter()
            .any(|row| row.id == provider_track && row.duplicate_of == library_track)
    );
    assert!(
        report
            .duplicates
            .iter()
            .any(|row| row.id == provider_album && row.duplicate_of == library_album)
    );
    assert!(
        report
            .duplicates
            .iter()
            .any(|row| row.id == provider_artist && row.duplicate_of == library_artist)
    );
    for (table, id) in [
        ("tracks", provider_track),
        ("albums", provider_album),
        ("artists", provider_artist),
    ] {
        let remaining: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE id = $1"))
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);
    }
    for (table, id) in [
        ("tracks", library_track),
        ("albums", library_album),
        ("artists", library_artist),
    ] {
        let remaining: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE id = $1"))
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 1);
    }
}

async fn seed_album_source(pool: &PgPool, album_id: Uuid, external_id: &str) {
    sqlx::query("INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', $3)")
        .bind(Uuid::now_v7())
        .bind(album_id)
        .bind(external_id)
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn provider_only_album_with_an_unmatched_track_is_excluded_from_comparison(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let library_artist = Uuid::now_v7();
    let provider_artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, 'Album Artist', 'album-artist-mbid')")
        .bind(library_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, 'Album Artist')")
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'album-artist-provider')")
        .bind(Uuid::now_v7())
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    let library_track = Uuid::now_v7();
    let provider_track = Uuid::now_v7();
    let unmatched_track = Uuid::now_v7();
    seed_legacy_track(
        &pool,
        library_track,
        "Album Match",
        library_artist,
        "Album Artist",
        ("local", "named:/music", "album-match.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        provider_track,
        "Album Match",
        provider_artist,
        "Album Artist",
        ("youtube_music", "global", "album-match-provider"),
    )
    .await;
    seed_legacy_track(
        &pool,
        unmatched_track,
        "Unmatched Member",
        library_artist,
        "Album Artist",
        ("youtube_music", "global", "unmatched-member-provider"),
    )
    .await;
    let target_album = Uuid::now_v7();
    let comparison_candidate = Uuid::now_v7();
    let duplicate_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, 'Shared Album', $2, 'shared-album-target')")
        .bind(target_album)
        .bind(library_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Shared Album', $2), ($3, 'Shared Album', $4)")
        .bind(comparison_candidate)
        .bind(library_artist)
        .bind(duplicate_album)
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    seed_album_source(&pool, comparison_candidate, "unmatched-album").await;
    seed_album_source(&pool, duplicate_album, "matching-album").await;
    sqlx::query("INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, 0), ($3, $4, 0), ($5, $6, 0)")
        .bind(target_album)
        .bind(library_track)
        .bind(comparison_candidate)
        .bind(unmatched_track)
        .bind(duplicate_album)
        .bind(provider_track)
        .execute(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(
        report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == provider_track)
    );
    assert!(
        report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == duplicate_album
                && duplicate.duplicate_of == target_album)
    );
    assert!(
        !report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == comparison_candidate)
    );
}

#[sqlx::test]
async fn an_empty_provider_album_matches_its_unique_library_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, 'Empty Album Artist', 'empty-album-artist')")
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    let library_album = Uuid::now_v7();
    let provider_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, 'Empty Album', $2, 'empty-album-release')")
        .bind(library_album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Empty Album', $2)")
        .bind(provider_album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    seed_album_source(&pool, provider_album, "empty-album-provider").await;

    let report = repository.provider_duplicates().await.unwrap();

    assert!(
        report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == provider_album
                && duplicate.duplicate_of == library_album)
    );
}

#[sqlx::test]
async fn ambiguous_album_artist_does_not_fall_back_to_an_uncredited_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist_a = Uuid::now_v7();
    let artist_b = Uuid::now_v7();
    let provider_album_artist = Uuid::now_v7();
    let track_artist = Uuid::now_v7();
    let provider_track_artist = Uuid::now_v7();
    for (id, name, mbid) in [
        (artist_a, "Ambiguous Album Artist", "ambiguous-artist-a"),
        (artist_b, "Ambiguous Album Artist", "ambiguous-artist-b"),
        (track_artist, "Track Artist", "track-artist"),
    ] {
        sqlx::query("INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(name)
            .bind(mbid)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, 'Ambiguous Album Artist'), ($2, 'Track Artist')")
        .bind(provider_album_artist)
        .bind(provider_track_artist)
        .execute(&pool)
        .await
        .unwrap();
    for (id, external_id) in [
        (provider_album_artist, "ambiguous-album-artist"),
        (provider_track_artist, "track-artist-provider"),
    ] {
        sqlx::query("INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', $3)")
            .bind(Uuid::now_v7())
            .bind(id)
            .bind(external_id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let local_track_a = Uuid::now_v7();
    let local_track_b = Uuid::now_v7();
    let library_match = Uuid::now_v7();
    let provider_match = Uuid::now_v7();
    seed_legacy_track(
        &pool,
        local_track_a,
        "Other A",
        artist_a,
        "Ambiguous Album Artist",
        ("local", "named:/music", "other-a.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        local_track_b,
        "Other B",
        artist_b,
        "Ambiguous Album Artist",
        ("local", "named:/music", "other-b.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        library_match,
        "Independent Match",
        track_artist,
        "Track Artist",
        ("local", "named:/music", "independent.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        provider_match,
        "Independent Match",
        provider_track_artist,
        "Track Artist",
        ("youtube_music", "global", "independent-provider"),
    )
    .await;
    let uncredited_album = Uuid::now_v7();
    let provider_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title) VALUES ($1, 'Uncredited Target')")
        .bind(uncredited_album)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Uncredited Target', $2)",
    )
    .bind(provider_album)
    .bind(provider_album_artist)
    .execute(&pool)
    .await
    .unwrap();
    seed_album_source(&pool, provider_album, "ambiguous-provider-album").await;
    sqlx::query("INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, 0)")
        .bind(provider_album)
        .bind(provider_match)
        .execute(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(
        report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == provider_match
                && duplicate.duplicate_of == library_match)
    );
    assert!(
        !report.duplicates.iter().any(|duplicate| duplicate.kind
            == crate::features::music_library::domain::DuplicateKind::Album)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM albums WHERE id = $1")
            .bind(uncredited_album)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn two_non_candidate_artists_keep_artist_and_album_duplicates_ambiguous(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_artist = Uuid::now_v7();
    let second_artist = Uuid::now_v7();
    let provider_artist = Uuid::now_v7();
    for (id, identifier) in [
        (first_artist, "same-name-1"),
        (second_artist, "same-name-2"),
    ] {
        sqlx::query(
            "INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, 'Same Artist', $2)",
        )
        .bind(id)
        .bind(identifier)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO artists (id, name) VALUES ($1, 'Same Artist')")
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'same-name-provider')")
        .bind(Uuid::now_v7())
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    seed_legacy_track(
        &pool,
        Uuid::now_v7(),
        "Ambiguous Song",
        first_artist,
        "Same Artist",
        ("local", "named:/music", "same-1.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        Uuid::now_v7(),
        "Ambiguous Song",
        second_artist,
        "Same Artist",
        ("local", "named:/music", "same-2.flac"),
    )
    .await;
    seed_legacy_track(
        &pool,
        Uuid::now_v7(),
        "Ambiguous Song",
        provider_artist,
        "Same Artist",
        ("youtube_music", "global", "same-provider-track"),
    )
    .await;
    let first_album = Uuid::now_v7();
    let second_album = Uuid::now_v7();
    let provider_album = Uuid::now_v7();
    for (id, artist_id) in [(first_album, first_artist), (second_album, second_artist)] {
        sqlx::query("INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, 'Same Album', $2, $3)")
            .bind(id)
            .bind(artist_id)
            .bind(format!("release-{id}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Same Album', $2)")
        .bind(provider_album)
        .bind(provider_artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'same-provider-album')")
        .bind(Uuid::now_v7())
        .bind(provider_album)
        .execute(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(report.duplicates.is_empty());
}

#[sqlx::test]
async fn provider_duplicates_lists_a_provider_track_matching_a_library_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (library, provider) =
        matching_provider_track(&repository, "Glue Song", "beabadoobee").await;

    let report = repository.provider_duplicates().await.unwrap();

    assert!(report.skipped.is_empty());
    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(report.duplicates[0].id, *provider.as_uuid());
    assert_eq!(report.duplicates[0].duplicate_of, *library.as_uuid());
    assert_eq!(report.duplicates[0].name, "Glue Song");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test]
async fn a_track_with_a_playlist_entry_is_skipped(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (_, provider) = matching_provider_track(&repository, "Playlist Song", "Artist").await;
    let user = Uuid::now_v7();
    insert_user(&pool, user, "playlist-owner").await;
    let playlist = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'mix')")
        .bind(playlist)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO playlist_entries (playlist_id, position, track_id) VALUES ($1, 0, $2)",
    )
    .bind(playlist)
    .bind(provider.as_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(report.duplicates.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(report.skipped[0].id, *provider.as_uuid());
    assert_eq!(
        report.skipped[0].reason,
        crate::features::music_library::domain::SkipReason::PlaylistEntry
    );
}

#[sqlx::test]
async fn a_track_with_a_queue_entry_is_skipped(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (_, provider) = matching_provider_track(&repository, "Queued Song", "Artist").await;
    let user = Uuid::now_v7();
    insert_user(&pool, user, "queue-owner").await;
    sqlx::query(
        "INSERT INTO restore_queues (user_id, current_index, changed_by) VALUES ($1, 0, $1)",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO restore_queue_entries (queue_user_id, position, track_id) VALUES ($1, 0, $2)",
    )
    .bind(user)
    .bind(provider.as_uuid())
    .execute(&pool)
    .await
    .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(report.duplicates.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(
        report.skipped[0].reason,
        crate::features::music_library::domain::SkipReason::QueueEntry
    );
}

#[sqlx::test]
async fn an_album_keeping_a_non_duplicate_track_is_not_reported_for_deletion(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (library, provider) = matching_provider_track(&repository, "Album Song", "Artist").await;
    let artist: Uuid = sqlx::query_scalar(
        "SELECT artist_id FROM track_artist_credits WHERE track_id = $1 AND position = 0",
    )
    .bind(library.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let target_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, 'Album', $2, 'rg-album')")
        .bind(target_album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    let provider_album = Uuid::now_v7();
    sqlx::query("INSERT INTO albums (id, title, album_artist_id) VALUES ($1, 'Album', $2)")
        .bind(provider_album)
        .bind(artist)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'provider-album')")
        .bind(Uuid::now_v7())
        .bind(provider_album)
        .execute(&pool)
        .await
        .unwrap();
    let unrelated = repository
        .get_or_create(source("unmatched-track"), metadata("Unmatched", "Artist"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, 0)")
        .bind(provider_album)
        .bind(unrelated.id().as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(
        report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == *provider.as_uuid())
    );
    assert!(
        !report
            .duplicates
            .iter()
            .any(|duplicate| duplicate.id == provider_album)
    );
}

#[sqlx::test]
async fn a_library_sourced_row_is_never_a_candidate(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (_, provider) = matching_provider_track(&repository, "Owned Duplicate", "Artist").await;
    sqlx::query("INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'local', 'named:/music', 'also-local')")
        .bind(Uuid::now_v7())
        .bind(provider.as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    assert!(report.duplicates.is_empty());
    assert!(report.skipped.is_empty());
}

#[sqlx::test]
async fn the_report_counts_listens(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (_, provider) = matching_provider_track(&repository, "Listened Song", "Artist").await;
    let user = Uuid::now_v7();
    insert_user(&pool, user, "listener").await;
    seed_play(&pool, user, &provider, chrono::Utc::now()).await;

    let report = repository.provider_duplicates().await.unwrap();

    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(report.duplicates[0].listens, 1);
}

#[sqlx::test]
async fn delete_removes_exactly_the_reported_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (library, provider) = matching_provider_track(&repository, "Delete Song", "Artist").await;

    let report = repository.delete_provider_duplicates().await.unwrap();

    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(report.duplicates[0].id, *provider.as_uuid());
    assert!(repository.find_track(&provider).await.unwrap().is_none());
    assert!(repository.find_track(&library).await.unwrap().is_some());
}

#[sqlx::test]
async fn provider_duplicates_changes_nothing(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (_, provider) = matching_provider_track(&repository, "Dry Run Song", "Artist").await;
    let before = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();

    let report = repository.provider_duplicates().await.unwrap();

    let after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(before, after);
    assert!(repository.find_track(&provider).await.unwrap().is_some());
}
