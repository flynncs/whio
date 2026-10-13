use std::sync::Arc;

use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceIdentity, SourceScope, StateTarget,
    TrackMetadata,
};
use crate::features::music_library::library_state::test_support::seed_play;
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, Retention, StateStore,
};
use sqlx::PgPool;
use tokio::sync::Barrier;
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

fn spotify_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("spotify".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn album_for(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("Hayley Williams".to_owned(), None).unwrap())
}

fn provider_album(title: &str, external_id: &str) -> AlbumMetadata {
    album_for(title).with_provider(Some(source(external_id)))
}

fn provider_album_without_artist(title: &str, external_id: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned())
        .unwrap()
        .with_provider(Some(source(external_id)))
}

fn scoped_source(external_id: &str, scope: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Named(scope.to_owned()),
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

    let report = repository.delete_provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

    assert!(report.duplicates.is_empty());
}

#[sqlx::test]
async fn provider_duplicates_lists_a_provider_track_matching_a_library_track(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (library, provider) =
        matching_provider_track(&repository, "Glue Song", "beabadoobee").await;

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(report.duplicates[0].listens, 1);
}

#[sqlx::test]
async fn delete_removes_exactly_the_reported_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (library, provider) = matching_provider_track(&repository, "Delete Song", "Artist").await;

    let report = repository.delete_provider_duplicates(&[]).await.unwrap();

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

    let report = repository.provider_duplicates(&[]).await.unwrap();

    let after = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM tracks")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(report.duplicates.len(), 1);
    assert_eq!(before, after);
    assert!(repository.find_track(&provider).await.unwrap().is_some());
}

async fn insert_artist(pool: &PgPool, id: Uuid, name: &str, mbid: Option<&str>) {
    sqlx::query("INSERT INTO artists (id, name, musicbrainz_artist_id) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(name)
        .bind(mbid)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_track(pool: &PgPool, id: Uuid, title: &str, duration_ms: i32) {
    sqlx::query("INSERT INTO tracks (id, title, duration_ms) VALUES ($1, $2, $3)")
        .bind(id)
        .bind(title)
        .bind(duration_ms)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_credit(
    pool: &PgPool,
    track_id: Uuid,
    position: i32,
    name: &str,
    artist_id: Uuid,
    role: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO track_artist_credits (track_id, position, name, artist_id, role) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(track_id)
    .bind(position)
    .bind(name)
    .bind(artist_id)
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_track_source(
    pool: &PgPool,
    track_id: Uuid,
    provider_id: &str,
    source_scope: &str,
    external_id: &str,
) {
    sqlx::query(
        "INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(track_id)
    .bind(provider_id)
    .bind(source_scope)
    .bind(external_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_artist_source(pool: &PgPool, artist_id: Uuid, external_id: &str) {
    sqlx::query(
        "INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(artist_id)
    .bind(external_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_album(
    pool: &PgPool,
    id: Uuid,
    title: &str,
    album_artist_id: Uuid,
    release_group: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO albums (id, title, album_artist_id, musicbrainz_release_group_id) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(title)
    .bind(album_artist_id)
    .bind(release_group)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_album_track(pool: &PgPool, album_id: Uuid, track_id: Uuid, position: i32) {
    sqlx::query("INSERT INTO album_tracks (album_id, track_id, position) VALUES ($1, $2, $3)")
        .bind(album_id)
        .bind(track_id)
        .bind(position)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_artist_favorite(
    pool: &PgPool,
    user: Uuid,
    artist_id: Uuid,
    starred_at: chrono::DateTime<chrono::Utc>,
) {
    sqlx::query(
        "INSERT INTO artist_favorites (user_id, artist_id, starred_at) VALUES ($1, $2, $3)",
    )
    .bind(user)
    .bind(artist_id)
    .bind(starred_at)
    .execute(pool)
    .await
    .unwrap();
}

async fn insert_artist_rating(pool: &PgPool, user: Uuid, artist_id: Uuid, rating: i16) {
    sqlx::query("INSERT INTO artist_ratings (user_id, artist_id, rating) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(artist_id)
        .bind(rating)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_album_favorite(
    pool: &PgPool,
    user: Uuid,
    album_id: Uuid,
    starred_at: chrono::DateTime<chrono::Utc>,
) {
    sqlx::query("INSERT INTO album_favorites (user_id, album_id, starred_at) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(album_id)
        .bind(starred_at)
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_album_rating(pool: &PgPool, user: Uuid, album_id: Uuid, rating: i16) {
    sqlx::query("INSERT INTO album_ratings (user_id, album_id, rating) VALUES ($1, $2, $3)")
        .bind(user)
        .bind(album_id)
        .bind(rating)
        .execute(pool)
        .await
        .unwrap();
}

async fn row_count(pool: &PgPool, table: &str, column: &str, id: Uuid) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE {column} = $1"))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM albums")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn provider_source_key_of(pool: &PgPool, album_id: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT provider_source_key FROM albums WHERE id = $1")
        .bind(album_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_source_owners_of(pool: &PgPool) -> Vec<(String, Uuid)> {
    sqlx::query_as("SELECT external_id, album_id FROM album_sources ORDER BY external_id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn insert_album_keyed(
    pool: &PgPool,
    id: Uuid,
    title: &str,
    album_artist_id: Uuid,
    key: &str,
) {
    sqlx::query(
        "INSERT INTO albums (id, title, album_artist_id, provider_source_key) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(title)
    .bind(album_artist_id)
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
}

async fn artist_source_owner(pool: &PgPool, external_id: &str) -> Uuid {
    sqlx::query_scalar("SELECT artist_id FROM artist_sources WHERE external_id = $1")
        .bind(external_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_artist_of(pool: &PgPool, album_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT album_artist_id FROM albums WHERE id = $1")
        .bind(album_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn credit_artist_at(pool: &PgPool, track_id: Uuid, position: i32) -> Option<Uuid> {
    sqlx::query_scalar(
        "SELECT artist_id FROM track_artist_credits WHERE track_id = $1 AND position = $2",
    )
    .bind(track_id)
    .bind(position)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn album_members(pool: &PgPool, album_id: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT track_id FROM album_tracks WHERE album_id = $1 ORDER BY track_id")
        .bind(album_id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn album_source_owners(pool: &PgPool, album_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT external_id FROM album_sources WHERE album_id = $1 ORDER BY external_id",
    )
    .bind(album_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn favorite_starred_at(
    pool: &PgPool,
    table: &str,
    column: &str,
    user: Uuid,
    target: Uuid,
) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar(&format!(
        "SELECT starred_at FROM {table} WHERE user_id = $1 AND {column} = $2"
    ))
    .bind(user)
    .bind(target)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn snapshot_rows(pool: &PgPool, table: &str) -> Vec<String> {
    sqlx::query_scalar(&format!(
        "SELECT to_jsonb(t)::text FROM {table} AS t ORDER BY to_jsonb(t)::text"
    ))
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn snapshot_tables(pool: &PgPool) -> Vec<(String, Vec<String>)> {
    let mut snapshot = Vec::new();
    for table in [
        "artists",
        "track_artist_credits",
        "track_genres",
        "artist_sources",
        "artist_favorites",
        "artist_ratings",
        "albums",
        "album_tracks",
        "album_sources",
        "album_favorites",
        "album_ratings",
        "tracks",
        "track_sources",
        "track_favorites",
        "track_ratings",
        "track_play_stats",
        "listens",
        "listen_deliveries",
        "playlists",
        "playlist_entries",
        "restore_queues",
        "restore_queue_entries",
        "provider_entity_state",
    ] {
        snapshot.push((table.to_owned(), snapshot_rows(pool, table).await));
    }
    snapshot
}

#[sqlx::test]
async fn prune_merges_same_album_tracks_moves_state_and_clears_short_album_state(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .get_or_create(source("first"), metadata("Stop The Wedding!", "Ashe"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(source("second"), metadata("Stop The Wedding!", "Ashe"))
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    let other = repository
        .resolve_or_create_album(&AlbumMetadata::new("Other".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_album"))
        .await
        .unwrap();
    repository
        .set_provider_entity_state(
            &source("MPREb_album"),
            &crate::features::music_library::domain::ProviderEntityState::new(
                chrono::Utc::now(),
                Some(2),
                None,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    for track in [&first, &second] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    repository
        .add_album_track(
            other.id(),
            second.id(),
            &AlbumTrack::new(Some(1), Some(2), 1).unwrap(),
        )
        .await
        .unwrap();
    let composer = Uuid::now_v7();
    insert_artist(&pool, composer, "Composer", Some("composer-id")).await;
    insert_credit(
        &pool,
        *second.id().as_uuid(),
        1,
        "Composer credited text",
        composer,
        Some("composer"),
    )
    .await;
    insert_credit(
        &pool,
        *second.id().as_uuid(),
        2,
        "Lyricist credited text",
        composer,
        Some("lyricist"),
    )
    .await;
    for (track, position, name) in [
        (&first, 0_i32, "Pop"),
        (&second, 0, "pop"),
        (&second, 1, "Soul"),
    ] {
        sqlx::query("INSERT INTO track_genres (track_id, position, name) VALUES ($1, $2, $3)")
            .bind(track.id().as_uuid())
            .bind(position)
            .bind(name)
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE tracks SET original_date = '2020-01-01', original_date_precision = 'year' WHERE id = $1")
        .bind(first.id().as_uuid()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE tracks SET recording_date = '2026-10-12', recording_date_precision = 'day', original_date = '2021-05-01', original_date_precision = 'month' WHERE id = $1")
        .bind(second.id().as_uuid()).execute(&pool).await.unwrap();
    let user = Uuid::now_v7();
    let another = Uuid::now_v7();
    insert_user(&pool, user, "kept").await;
    insert_user(&pool, another, "moved").await;
    for (user_id, track, rating) in [
        (user, &first, 5_u8),
        (user, &second, 1),
        (another, &second, 3),
    ] {
        repository
            .set_starred(user_id, StateTarget::Track(track.id()), true)
            .await
            .unwrap();
        repository
            .set_rating(user_id, StateTarget::Track(track.id()), Some(rating))
            .await
            .unwrap();
    }
    let start: chrono::DateTime<chrono::Utc> = "2026-10-12T00:00:00Z".parse().unwrap();
    for (minute, user_id, track) in [
        (0, user, &first),
        (1, user, &first),
        (2, user, &second),
        (3, user, &second),
        (4, user, &second),
        (5, another, &second),
    ] {
        seed_play(
            &pool,
            user_id,
            track.id(),
            start + chrono::Duration::minutes(minute),
        )
        .await;
    }
    let destination = Uuid::now_v7();
    let kept_delivery = Uuid::now_v7();
    let moved_delivery = Uuid::now_v7();
    for (id, track, minute, state, reference) in [
        (kept_delivery, &first, 10, "delivered", "kept-reference"),
        (
            Uuid::now_v7(),
            &second,
            10,
            "pending",
            "conflicting-reference",
        ),
        (moved_delivery, &second, 11, "rejected", "moved-reference"),
    ] {
        sqlx::query(
            "INSERT INTO listen_deliveries (id, user_id, origin, track_id, listen_at, session_key, destination_id, provider_id, state, provider_idempotence, provider_reference, created_at, updated_at) VALUES ($1, $2, 'native', $3, $4, '', $5, 'listenbrainz', $6, 'timestamp_only', $7, $4, $4)",
        ).bind(id).bind(user).bind(track.id().as_uuid()).bind(start + chrono::Duration::minutes(minute)).bind(destination).bind(state).bind(reference).execute(&pool).await.unwrap();
    }
    let playlist = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, user_id, name) VALUES ($1, $2, 'mix')")
        .bind(playlist)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    for (position, track) in [(0_i32, &first), (1, &second), (2, &first), (3, &second)] {
        sqlx::query(
            "INSERT INTO playlist_entries (playlist_id, position, track_id) VALUES ($1, $2, $3)",
        )
        .bind(playlist)
        .bind(position)
        .bind(track.id().as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    }
    let playlist_entries: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT entry_id, position FROM playlist_entries WHERE playlist_id = $1 ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO restore_queues (user_id, changed_by) VALUES ($1, $1)")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO restore_queue_entries (queue_user_id, position, track_id) VALUES ($1, 0, $2)",
    )
    .bind(user)
    .bind(second.id().as_uuid())
    .execute(&pool)
    .await
    .unwrap();
    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(snapshot_tables(&pool).await, before);
    assert_eq!(preview.track_merges.len(), 1);
    assert_eq!(preview.track_merges[0].id, *second.id().as_uuid());
    assert_eq!(preview.track_merges[0].into, *first.id().as_uuid());
    assert_eq!(preview.refetch.len(), 1);
    let applied = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(preview, applied);
    assert_eq!(
        row_count(&pool, "tracks", "id", *second.id().as_uuid()).await,
        0
    );
    assert_eq!(
        row_count(&pool, "track_sources", "track_id", *first.id().as_uuid()).await,
        2
    );
    assert_eq!(
        album_members(&pool, *other.id().as_uuid()).await,
        [*first.id().as_uuid()]
    );
    assert_eq!(
        row_count(&pool, "track_favorites", "track_id", *first.id().as_uuid()).await,
        2
    );
    let ratings: Vec<i16> =
        sqlx::query_scalar("SELECT rating FROM track_ratings WHERE track_id = $1 ORDER BY rating")
            .bind(first.id().as_uuid())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(ratings, [3, 5]);
    assert_eq!(
        row_count(&pool, "track_play_stats", "track_id", *first.id().as_uuid()).await,
        2
    );
    assert_eq!(
        row_count(&pool, "listens", "track_id", *first.id().as_uuid()).await,
        6
    );
    let stats: Vec<(Uuid, i32, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as(
        "SELECT user_id, play_count, last_played_at FROM track_play_stats WHERE track_id = $1",
    )
    .bind(first.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(stats.contains(&(user, 5, Some(start + chrono::Duration::minutes(4)))));
    assert!(stats.contains(&(another, 1, Some(start + chrono::Duration::minutes(5)))));
    let deliveries: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, state, provider_reference FROM listen_deliveries WHERE track_id = $1",
    )
    .bind(first.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(deliveries.len(), 2);
    assert!(deliveries.contains(&(
        kept_delivery,
        "delivered".to_owned(),
        "kept-reference".to_owned()
    )));
    assert!(deliveries.contains(&(
        moved_delivery,
        "rejected".to_owned(),
        "moved-reference".to_owned()
    )));
    let credits: Vec<(i32, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT position, name, artist_id, role FROM track_artist_credits WHERE track_id = $1 ORDER BY position",
    ).bind(first.id().as_uuid()).fetch_all(&pool).await.unwrap();
    assert_eq!(credits.len(), 3);
    assert_eq!(
        credits[1],
        (
            1,
            "Composer credited text".to_owned(),
            Some(composer),
            Some("composer".to_owned())
        )
    );
    assert_eq!(
        credits[2],
        (
            2,
            "Lyricist credited text".to_owned(),
            Some(composer),
            Some("lyricist".to_owned())
        )
    );
    let genres: Vec<(i32, String)> = sqlx::query_as(
        "SELECT position, name FROM track_genres WHERE track_id = $1 ORDER BY position",
    )
    .bind(first.id().as_uuid())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(genres, [(0, "Pop".to_owned()), (1, "Soul".to_owned())]);
    let dates: (Option<String>, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT recording_date::TEXT, recording_date_precision, original_date::TEXT, original_date_precision FROM tracks WHERE id = $1",
    ).bind(first.id().as_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        dates,
        (
            Some("2026-10-12".to_owned()),
            Some("day".to_owned()),
            Some("2020-01-01".to_owned()),
            Some("year".to_owned())
        )
    );
    assert_eq!(
        row_count(&pool, "playlist_entries", "track_id", *first.id().as_uuid()).await,
        4
    );
    let retained_entries: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT entry_id, position FROM playlist_entries WHERE playlist_id = $1 ORDER BY position",
    )
    .bind(playlist)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(retained_entries, playlist_entries);
    assert_eq!(
        row_count(
            &pool,
            "restore_queue_entries",
            "track_id",
            *first.id().as_uuid()
        )
        .await,
        1
    );
    assert!(
        snapshot_rows(&pool, "provider_entity_state")
            .await
            .is_empty()
    );
    assert!(
        repository
            .provider_duplicates(&[])
            .await
            .unwrap()
            .track_merges
            .is_empty()
    );
}

#[sqlx::test]
async fn prune_splits_one_provider_onto_each_of_its_albums(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Ego Death".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_album"))
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_single"))
        .await
        .unwrap();
    let member = repository
        .get_or_create(source("vid-1"), metadata("Title Track", "Ashe"))
        .await
        .unwrap();
    repository
        .add_album_track(album.id(), member.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    for external in ["MPREb_album", "MPREb_single"] {
        repository
            .set_provider_entity_state(
                &source(external),
                &crate::features::music_library::domain::ProviderEntityState::new(
                    chrono::Utc::now(),
                    Some(2),
                    None,
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let before = snapshot_tables(&pool).await;

    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(snapshot_tables(&pool).await, before);
    assert_eq!(preview.splits.len(), 1);
    assert_eq!(preview.splits[0].id, *album.id().as_uuid());
    assert_eq!(preview.splits[0].name, "Ego Death");
    assert_eq!(preview.splits[0].provider, "youtube_music");
    assert_eq!(preview.splits[0].external_id, "MPREb_single");

    let applied = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(applied.splits.len(), 1);
    assert_eq!(applied.splits[0].provider, preview.splits[0].provider);
    assert_eq!(applied.splits[0].external_id, preview.splits[0].external_id);
    let split_album = applied.splits[0].into;

    assert_eq!(
        album_source_owners(&pool, *album.id().as_uuid()).await,
        vec!["MPREb_album".to_owned()]
    );
    assert_eq!(
        album_source_owners(&pool, split_album).await,
        vec!["MPREb_single".to_owned()]
    );
    assert_eq!(
        album_members(&pool, *album.id().as_uuid()).await,
        vec![*member.id().as_uuid()]
    );
    assert!(album_members(&pool, split_album).await.is_empty());
    assert_eq!(album_artist_of(&pool, split_album).await, None);
    assert!(
        snapshot_rows(&pool, "provider_entity_state")
            .await
            .is_empty()
    );
    assert!(
        repository
            .provider_duplicates(&[])
            .await
            .unwrap()
            .splits
            .is_empty()
    );
}

#[sqlx::test]
async fn prune_leaves_an_album_with_sources_from_two_providers_alone(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Ego Death".to_owned()).unwrap())
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &source("MPREb_album"))
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &spotify_source("sp-1"))
        .await
        .unwrap();

    let report = repository.delete_provider_duplicates(&[]).await.unwrap();

    assert!(report.splits.is_empty());
    assert_eq!(
        album_source_owners(&pool, *album.id().as_uuid()).await,
        vec!["MPREb_album".to_owned(), "sp-1".to_owned()]
    );
}

async fn race_two_provider_ids(repository: &Arc<PostgresTrackRepository>) -> Vec<Uuid> {
    let barrier = Arc::new(Barrier::new(2));
    let mut tasks = Vec::new();
    for external_id in ["MPREb_first", "MPREb_second"] {
        let repository = Arc::clone(repository);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .resolve_or_create_album(&provider_album("Ego Death", external_id))
                .await
                .map(|album| *album.id().as_uuid())
        }));
    }

    let mut resolved = Vec::new();
    for task in tasks {
        resolved.push(task.await.unwrap().unwrap());
    }
    resolved
}

async fn wait_until_blocked(pool: &PgPool, holder_pid: i32) {
    let observed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> $1 AND wait_event_type = 'Lock'",
            )
            .bind(holder_pid)
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        observed.is_ok(),
        "the competing resolution must block on the held claim"
    );
}

#[sqlx::test]
async fn two_provider_ids_racing_for_a_new_album_keep_one_row_each(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    let resolved = race_two_provider_ids(&repository).await;

    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(owners.len(), 2);
    assert_eq!(owners[0].0, "MPREb_first");
    assert_eq!(owners[1].0, "MPREb_second");
    assert_ne!(owners[0].1, owners[1].1);
    assert!(resolved.contains(&owners[0].1));
    assert!(resolved.contains(&owners[1].1));
    let keys: Vec<Option<String>> =
        sqlx::query_scalar("SELECT provider_source_key FROM albums ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(keys.contains(&Some("youtube_music:MPREb_first".to_owned())));
    assert!(keys.contains(&Some("youtube_music:MPREb_second".to_owned())));
}

#[sqlx::test]
async fn two_provider_ids_racing_for_one_library_album_keep_one_row_each(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    let library_id = *library.id().as_uuid();

    race_two_provider_ids(&repository).await;

    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(owners.len(), 2);
    assert_ne!(owners[0].1, owners[1].1);
    assert!(owners.iter().any(|(_, album_id)| *album_id == library_id));
    assert_eq!(
        provider_source_key_of(&pool, library_id).await,
        None,
        "the library row keeps the text slot"
    );
    let keyed_id = owners
        .iter()
        .find(|(_, album_id)| *album_id != library_id)
        .unwrap()
        .1;
    let keyed_source = owners
        .iter()
        .find(|(_, album_id)| *album_id == keyed_id)
        .unwrap()
        .0
        .clone();
    assert_eq!(
        provider_source_key_of(&pool, keyed_id).await,
        Some(format!("youtube_music:{keyed_source}"))
    );
}

#[sqlx::test]
async fn a_held_library_row_forces_the_competing_provider_id_onto_a_new_row(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    let library_id = *library.id().as_uuid();

    let mut holder = pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    sqlx::query("SELECT id FROM albums WHERE id = $1 FOR UPDATE")
        .bind(library_id)
        .execute(&mut *holder)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'MPREb_first')",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .execute(&mut *holder)
    .await
    .unwrap();

    let contender = {
        let repository = Arc::clone(&repository);
        tokio::spawn(async move {
            repository
                .resolve_or_create_album(&provider_album("Ego Death", "MPREb_second"))
                .await
                .map(|album| *album.id().as_uuid())
        })
    };

    wait_until_blocked(&pool, holder_pid).await;
    holder.commit().await.unwrap();

    let contender_id = contender.await.unwrap().unwrap();
    assert_ne!(contender_id, library_id);
    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(owners.len(), 2);
    assert_eq!(
        owners
            .iter()
            .find(|(external_id, _)| external_id == "MPREb_first")
            .unwrap()
            .1,
        library_id
    );
    assert_eq!(
        owners
            .iter()
            .find(|(external_id, _)| external_id == "MPREb_second")
            .unwrap()
            .1,
        contender_id
    );
    assert_eq!(provider_source_key_of(&pool, library_id).await, None);
    assert_eq!(
        provider_source_key_of(&pool, contender_id).await,
        Some("youtube_music:MPREb_second".to_owned())
    );
}

#[sqlx::test]
async fn a_held_new_provider_claim_serialises_the_competing_resolution(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    let mut holder = pool.begin().await.unwrap();
    let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    let album_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, title, album_artist_id, provider_source_key) VALUES ($1, 'Ego Death', NULL, 'youtube_music:MPREb_held')",
    )
    .bind(album_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'MPREb_held')",
    )
    .bind(Uuid::now_v7())
    .bind(album_id)
    .execute(&mut *holder)
    .await
    .unwrap();

    let contender = {
        let repository = Arc::clone(&repository);
        tokio::spawn(async move {
            repository
                .resolve_or_create_album(&provider_album_without_artist("Ego Death", "MPREb_held"))
                .await
                .map(|album| *album.id().as_uuid())
        })
    };

    wait_until_blocked(&pool, holder_pid).await;
    holder.commit().await.unwrap();

    let resolved = contender.await.unwrap().unwrap();
    assert_eq!(resolved, album_id);
    assert_eq!(album_count(&pool).await, 1);
    assert_eq!(
        provider_source_key_of(&pool, album_id).await,
        Some("youtube_music:MPREb_held".to_owned())
    );
    assert_eq!(
        album_source_owners(&pool, album_id).await,
        vec!["MPREb_held".to_owned()]
    );
}

#[sqlx::test]
async fn a_different_provider_id_does_not_wait_for_an_uncommitted_claim(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    let mut holder = pool.begin().await.unwrap();
    let album_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, title, album_artist_id, provider_source_key) VALUES ($1, 'Ego Death', NULL, 'youtube_music:MPREb_held')",
    )
    .bind(album_id)
    .execute(&mut *holder)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id) VALUES ($1, $2, 'youtube_music', 'global', 'MPREb_held')",
    )
    .bind(Uuid::now_v7())
    .bind(album_id)
    .execute(&mut *holder)
    .await
    .unwrap();

    let resolved = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        repository
            .resolve_or_create_album(&provider_album_without_artist("Ego Death", "MPREb_other")),
    )
    .await;
    assert!(
        resolved.is_ok(),
        "a different provider id must not wait for an uncommitted claim"
    );
    let other_id = *resolved.unwrap().unwrap().id().as_uuid();
    assert_ne!(other_id, album_id);

    holder.commit().await.unwrap();

    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(owners.len(), 2);
    for (external_id, owner) in owners {
        assert_eq!(
            provider_source_key_of(&pool, owner).await,
            Some(format!("youtube_music:{external_id}"))
        );
    }
}

#[sqlx::test]
async fn scoped_associations_of_one_provider_id_stay_on_one_album(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let first = repository
        .resolve_or_create_album(
            &album_for("Ego Death").with_provider(Some(scoped_source("MPREb_scoped", "one"))),
        )
        .await
        .unwrap();
    let second = repository
        .resolve_or_create_album(
            &album_for("Ego Death").with_provider(Some(scoped_source("MPREb_scoped", "two"))),
        )
        .await
        .unwrap();

    assert_eq!(first.id(), second.id());
    assert_eq!(album_count(&pool).await, 1);
    assert_eq!(
        provider_source_key_of(&pool, *first.id().as_uuid()).await,
        Some("youtube_music:MPREb_scoped".to_owned())
    );
    assert_eq!(album_source_owners_of(&pool).await.len(), 2);

    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert!(preview.splits.is_empty());
    assert_eq!(snapshot_tables(&pool).await, before);
    let applied = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert!(applied.splits.is_empty());
    assert_eq!(album_count(&pool).await, 1);
    assert_eq!(album_source_owners_of(&pool).await.len(), 2);
}

#[sqlx::test]
async fn a_second_scope_after_a_split_returns_the_provider_first_album(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_album"))
        .await
        .unwrap();
    let album_id = *album.id().as_uuid();
    let single = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_single"))
        .await
        .unwrap();
    assert_ne!(album_id, *single.id().as_uuid());

    let scoped = repository
        .resolve_or_create_album(
            &album_for("Ego Death").with_provider(Some(scoped_source("MPREb_album", "second"))),
        )
        .await
        .unwrap();

    assert_eq!(*scoped.id().as_uuid(), album_id);
    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(
        owners
            .iter()
            .filter(|(external_id, owner)| external_id == "MPREb_album" && *owner == album_id)
            .count(),
        2
    );
    assert_eq!(
        provider_source_key_of(&pool, album_id).await,
        Some("youtube_music:MPREb_album".to_owned())
    );
    assert_eq!(
        album_source_owners(&pool, *single.id().as_uuid()).await,
        vec!["MPREb_single".to_owned()]
    );
}

#[sqlx::test]
async fn a_second_scope_after_a_split_returns_the_library_first_album(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    let library_id = *library.id().as_uuid();
    let joined = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_album"))
        .await
        .unwrap();
    assert_eq!(*joined.id().as_uuid(), library_id);
    let single = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_single"))
        .await
        .unwrap();
    assert_ne!(library_id, *single.id().as_uuid());

    let scoped = repository
        .resolve_or_create_album(
            &album_for("Ego Death").with_provider(Some(scoped_source("MPREb_album", "second"))),
        )
        .await
        .unwrap();

    assert_eq!(*scoped.id().as_uuid(), library_id);
    assert_eq!(album_count(&pool).await, 2);
    let owners = album_source_owners_of(&pool).await;
    assert_eq!(
        owners
            .iter()
            .filter(|(external_id, owner)| external_id == "MPREb_album" && *owner == library_id)
            .count(),
        2
    );
    assert_eq!(provider_source_key_of(&pool, library_id).await, None);
    assert_eq!(
        album_source_owners(&pool, *single.id().as_uuid()).await,
        vec!["MPREb_single".to_owned()]
    );
}

#[sqlx::test]
async fn library_resolution_after_a_provider_split_finds_the_null_key_slot(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let legacy = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_album"))
        .await
        .unwrap();
    repository
        .set_album_source(legacy.id(), &source("MPREb_single"))
        .await
        .unwrap();
    let report = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(report.splits.len(), 1);
    assert_eq!(album_count(&pool).await, 2);

    let barrier = Arc::new(Barrier::new(4));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let repository = Arc::clone(&repository);
        let barrier = Arc::clone(&barrier);
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            repository
                .resolve_or_create_album(&album_for("Ego Death"))
                .await
                .map(|album| *album.id().as_uuid())
        }));
    }
    let mut resolved = Vec::new();
    for task in tasks {
        resolved.push(task.await.unwrap().unwrap());
    }

    assert!(resolved.iter().all(|id| id == &resolved[0]));
    assert_eq!(album_count(&pool).await, 3);
    let null_keys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM albums WHERE provider_source_key IS NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(null_keys, 1);

    let again = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    assert_eq!(*again.id().as_uuid(), resolved[0]);
    assert_eq!(album_count(&pool).await, 3);
}

#[sqlx::test]
async fn a_library_album_resolved_first_keeps_its_text_slot(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    let library_id = *library.id().as_uuid();

    let first = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_album"))
        .await
        .unwrap();
    assert_eq!(*first.id().as_uuid(), library_id);
    let second = repository
        .resolve_or_create_album(&provider_album("Ego Death", "MPREb_single"))
        .await
        .unwrap();
    assert_ne!(*second.id().as_uuid(), library_id);

    let again = repository
        .resolve_or_create_album(&album_for("Ego Death"))
        .await
        .unwrap();
    assert_eq!(*again.id().as_uuid(), library_id);
    assert_eq!(provider_source_key_of(&pool, library_id).await, None);
    assert_eq!(album_count(&pool).await, 2);
}

#[sqlx::test]
async fn prune_keeps_the_keyed_source_when_an_artist_merge_adds_an_older_one(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let identified = Uuid::now_v7();
    let id_less = Uuid::now_v7();
    insert_artist(&pool, identified, "Shared Name", Some("mb-shared")).await;
    insert_artist(&pool, id_less, "Shared Name", None).await;
    insert_artist_source(&pool, id_less, "shared-provider").await;

    let library_track = Uuid::now_v7();
    insert_track(&pool, library_track, "Library Song", 180_000).await;
    insert_credit(&pool, library_track, 0, "Shared Name", identified, None).await;
    insert_credit(
        &pool,
        library_track,
        1,
        "Shared Name",
        id_less,
        Some("composer"),
    )
    .await;
    insert_track_source(
        &pool,
        library_track,
        "local",
        "named:/music",
        "library-song.flac",
    )
    .await;

    let provider_track = Uuid::now_v7();
    insert_track(&pool, provider_track, "Provider Song", 200_000).await;
    insert_credit(&pool, provider_track, 0, "Shared Name", id_less, None).await;

    let target_album = Uuid::now_v7();
    insert_album_keyed(
        &pool,
        target_album,
        "Switchblade",
        identified,
        "youtube_music:later",
    )
    .await;
    seed_album_source(&pool, target_album, "later").await;
    insert_album_track(&pool, target_album, library_track, 0).await;

    let source_album = Uuid::now_v7();
    insert_album_keyed(
        &pool,
        source_album,
        "Switchblade",
        id_less,
        "youtube_music:earlier",
    )
    .await;
    seed_album_source(&pool, source_album, "earlier").await;
    for (external_id, created_at) in [
        ("earlier", "2026-01-01T00:00:00Z"),
        ("later", "2026-02-01T00:00:00Z"),
    ] {
        sqlx::query("UPDATE album_sources SET created_at = $1::TIMESTAMPTZ WHERE external_id = $2")
            .bind(created_at)
            .bind(external_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(
        snapshot_tables(&pool).await,
        before,
        "the dry run rolls back"
    );
    assert_eq!(preview.merges.len(), 1);
    assert_eq!(preview.splits.len(), 1);
    assert_eq!(preview.splits[0].id, target_album);
    assert_eq!(preview.splits[0].external_id, "earlier");

    let applied = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(applied.splits.len(), 1);
    assert_eq!(applied.splits[0].external_id, "earlier");

    assert_eq!(row_count(&pool, "albums", "id", source_album).await, 0);
    assert_eq!(
        album_source_owners(&pool, target_album).await,
        vec!["later".to_owned()]
    );
    assert_eq!(
        provider_source_key_of(&pool, target_album).await,
        Some("youtube_music:later".to_owned())
    );
    let split_album = applied.splits[0].into;
    assert_eq!(
        album_source_owners(&pool, split_album).await,
        vec!["earlier".to_owned()]
    );
    assert_eq!(
        provider_source_key_of(&pool, split_album).await,
        Some("youtube_music:earlier".to_owned())
    );
    assert_eq!(
        row_count(&pool, "album_tracks", "album_id", target_album).await,
        1
    );
    assert_eq!(
        row_count(&pool, "album_tracks", "album_id", split_album).await,
        0
    );
}

#[sqlx::test]
async fn prune_preserves_source_only_performer_credits_and_absent_original_date(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .get_or_create(
            source("first"),
            TrackMetadata::from_names("Song".to_owned(), vec!["Artist".to_owned()], Some(200_000))
                .unwrap(),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            source("second"),
            TrackMetadata::from_names(
                "Song".to_owned(),
                vec!["Artist".to_owned(), "Guest credited text".to_owned()],
                Some(200_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    for track in [&first, &second] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let guest: Uuid = sqlx::query_scalar(
        "SELECT artist_id FROM track_artist_credits WHERE track_id = $1 AND position = 1",
    )
    .bind(second.id().as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE tracks SET recording_date = '2020-01-01', recording_date_precision = 'year' WHERE id = $1").bind(first.id().as_uuid()).execute(&pool).await.unwrap();
    sqlx::query("UPDATE tracks SET recording_date = '2021-01-01', recording_date_precision = 'day', original_date = '2019-05-01', original_date_precision = 'month' WHERE id = $1").bind(second.id().as_uuid()).execute(&pool).await.unwrap();
    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(snapshot_tables(&pool).await, before);
    assert_eq!(preview.track_merges.len(), 1);
    assert_eq!(
        repository.delete_provider_duplicates(&[]).await.unwrap(),
        preview
    );
    let moved: (String, Uuid, Option<String>) = sqlx::query_as("SELECT name, artist_id, role FROM track_artist_credits WHERE track_id = $1 AND position = 1").bind(first.id().as_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(moved, ("Guest credited text".to_owned(), guest, None));
    let dates: (Option<String>, Option<String>, Option<String>, Option<String>) = sqlx::query_as("SELECT recording_date::TEXT, recording_date_precision, original_date::TEXT, original_date_precision FROM tracks WHERE id = $1").bind(first.id().as_uuid()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        dates,
        (
            Some("2020-01-01".to_owned()),
            Some("year".to_owned()),
            Some("2019-05-01".to_owned()),
            Some("month".to_owned())
        )
    );
}

#[sqlx::test]
async fn prune_skips_overlapping_listen_histories_without_changing_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .get_or_create(source("first"), metadata("Song", "Artist"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(source("second"), metadata("Song", "Artist"))
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    for track in [&first, &second] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let user = Uuid::now_v7();
    insert_user(&pool, user, "listener").await;
    let at = "2026-10-12T00:00:00Z".parse().unwrap();
    seed_play(&pool, user, first.id(), at).await;
    seed_play(&pool, user, second.id(), at).await;
    seed_play(
        &pool,
        user,
        second.id(),
        "2026-10-12T00:01:00Z".parse().unwrap(),
    )
    .await;
    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(snapshot_tables(&pool).await, before);
    assert!(preview.track_merges.is_empty());
    assert_eq!(preview.skipped.len(), 1);
    assert_eq!(preview.skipped[0].id, *second.id().as_uuid());
    assert_eq!(
        preview.skipped[0].reason,
        crate::features::music_library::domain::SkipReason::OverlappingListens
    );
    assert_eq!(
        repository.delete_provider_duplicates(&[]).await.unwrap(),
        preview
    );
    assert_eq!(snapshot_tables(&pool).await, before);
}

#[sqlx::test]
async fn protected_provider_library_duplicates_are_skipped(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = metadata("Song", "Artist");
    repository
        .get_or_create(local_source("local.flac"), metadata.clone())
        .await
        .unwrap();
    let provider = repository
        .get_or_create(source("provider"), metadata)
        .await
        .unwrap();
    let protected = [provider.id().clone()];
    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&protected).await.unwrap();
    assert_eq!(snapshot_tables(&pool).await, before);
    assert!(preview.duplicates.is_empty());
    assert_eq!(preview.skipped.len(), 1);
    assert_eq!(
        preview.skipped[0].reason,
        crate::features::music_library::domain::SkipReason::AcquisitionOrigin
    );
    assert_eq!(
        repository
            .delete_provider_duplicates(&protected)
            .await
            .unwrap(),
        preview
    );
    assert_eq!(snapshot_tables(&pool).await, before);
}

#[sqlx::test]
async fn prune_does_not_merge_outside_tolerance_separate_albums_or_identified_tracks(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    let other = repository
        .resolve_or_create_album(&AlbumMetadata::new("Other".to_owned()).unwrap())
        .await
        .unwrap();
    for (video, title, duration, membership) in [
        ("short", "Song", 199_000, &album),
        ("long", "Song", 201_001, &album),
        ("other", "Song", 199_000, &other),
        ("identified", "Identified", 199_000, &album),
        ("identified-2", "Identified", 199_000, &album),
    ] {
        let mut metadata =
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(duration))
                .unwrap();
        if video.starts_with("identified") {
            metadata = metadata.with_recording_id(Some(video.to_owned())).unwrap();
        }
        let track = repository
            .get_or_create(source(video), metadata)
            .await
            .unwrap();
        repository
            .add_album_track(membership.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let before = snapshot_tables(&pool).await;
    let report = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert!(report.track_merges.is_empty());
    assert_eq!(snapshot_tables(&pool).await, before);
}

#[sqlx::test]
async fn prune_can_keep_an_older_identified_provider_member(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let metadata = metadata("Same Song", "Artist");
    let older = repository
        .get_or_create(
            source("identified"),
            metadata
                .clone()
                .with_recording_id(Some(Uuid::from_u128(42).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let newer = repository
        .get_or_create(source("unidentified"), metadata)
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();
    for track in [&older, &newer] {
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let report = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(report.track_merges.len(), 1);
    assert_eq!(report.track_merges[0].into, *older.id().as_uuid());
    assert_eq!(
        row_count(&pool, "tracks", "id", *newer.id().as_uuid()).await,
        0
    );
    let recording_id: Option<String> =
        sqlx::query_scalar("SELECT musicbrainz_recording_id FROM tracks WHERE id = $1")
            .bind(older.id().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recording_id, Some(Uuid::from_u128(42).to_string()));
}

#[sqlx::test]
async fn prune_leaves_ambiguous_album_tracks_and_settled_or_unknown_counts_alone(pool: PgPool) {
    use crate::features::music_library::domain::ProviderEntityState;
    let repository = PostgresTrackRepository::new(pool.clone());
    for (name, count, expected_clear) in [
        ("unknown", None, false),
        ("empty", Some(0), false),
        ("short", Some(1), true),
        ("settled", Some(3), false),
    ] {
        let album = repository
            .resolve_or_create_album(&AlbumMetadata::new(name.to_owned()).unwrap())
            .await
            .unwrap();
        let album_source = source(&format!("MPREb_{name}"));
        repository
            .set_album_source(album.id(), &album_source)
            .await
            .unwrap();
        repository
            .set_provider_entity_state(
                &album_source,
                &ProviderEntityState::new(chrono::Utc::now(), count, None).unwrap(),
            )
            .await
            .unwrap();
        if name == "settled" {
            for video in ["first", "second", "third"] {
                let track = repository
                    .get_or_create(source(video), metadata("Same Song", "Artist"))
                    .await
                    .unwrap();
                repository
                    .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
                    .await
                    .unwrap();
            }
        }
        let preview = repository.provider_duplicates(&[]).await.unwrap();
        assert!(preview.track_merges.is_empty());
        assert_eq!(
            preview
                .refetch
                .iter()
                .any(|refetch| &refetch.id == album.id().as_uuid()),
            expected_clear
        );
        repository.delete_provider_duplicates(&[]).await.unwrap();
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM provider_entity_state WHERE external_id = $1")
                .bind(album_source.external_id())
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, i64::from(!expected_clear));
    }
}

#[sqlx::test]
async fn prune_merges_a_credited_name_only_artist_into_its_identified_namesake(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let identified = Uuid::now_v7();
    let id_less = Uuid::now_v7();
    insert_artist(&pool, identified, "beabadoobee", Some("mb-bea")).await;
    insert_artist(&pool, id_less, "beabadoobee", None).await;
    insert_artist_source(&pool, id_less, "bea-provider").await;

    let library_track = Uuid::now_v7();
    insert_track(&pool, library_track, "Glue Song", 240_000).await;
    insert_credit(&pool, library_track, 0, "beabadoobee", identified, None).await;
    insert_credit(
        &pool,
        library_track,
        1,
        "beabadoobee",
        id_less,
        Some("composer"),
    )
    .await;
    insert_track_source(&pool, library_track, "local", "named:/music", "glue.flac").await;

    let provider_track = Uuid::now_v7();
    insert_track(&pool, provider_track, "Glue Song", 240_000).await;
    insert_credit(&pool, provider_track, 0, "beabadoobee", id_less, None).await;
    insert_track_source(
        &pool,
        provider_track,
        "youtube_music",
        "global",
        "glue-video",
    )
    .await;

    let library_album = Uuid::now_v7();
    insert_album(
        &pool,
        library_album,
        "Beatopia",
        identified,
        Some("rg-beatopia"),
    )
    .await;
    insert_album_track(&pool, library_album, library_track, 0).await;

    let provider_album = Uuid::now_v7();
    insert_album(&pool, provider_album, "Beatopia", id_less, None).await;
    seed_album_source(&pool, provider_album, "beatopia-provider").await;
    insert_album_track(&pool, provider_album, provider_track, 0).await;

    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();

    assert_eq!(preview.merges.len(), 1);
    assert_eq!(preview.merges[0].id, id_less);
    assert_eq!(preview.merges[0].name, "beabadoobee");
    assert_eq!(preview.merges[0].into, identified);
    assert_eq!(preview.merges[0].credits, 2);
    assert_eq!(preview.merges[0].albums_moved, 1);
    assert_eq!(preview.merges[0].albums_merged, 0);
    assert!(
        preview
            .duplicates
            .iter()
            .any(|row| row.id == provider_track && row.duplicate_of == library_track)
    );
    assert!(
        preview
            .duplicates
            .iter()
            .any(|row| row.id == provider_album && row.duplicate_of == library_album)
    );

    assert_eq!(row_count(&pool, "artists", "id", id_less).await, 1);
    assert_eq!(album_artist_of(&pool, provider_album).await, Some(id_less));
    assert_eq!(
        credit_artist_at(&pool, library_track, 1).await,
        Some(id_less)
    );
    assert_eq!(artist_source_owner(&pool, "bea-provider").await, id_less);
    assert_eq!(
        snapshot_tables(&pool).await,
        before,
        "the dry run rolls every table back"
    );

    let applied = repository.delete_provider_duplicates(&[]).await.unwrap();

    assert_eq!(applied, preview, "apply returns the previewed report");
    assert_eq!(row_count(&pool, "artists", "id", id_less).await, 0);
    assert_eq!(row_count(&pool, "tracks", "id", provider_track).await, 0);
    assert_eq!(row_count(&pool, "albums", "id", provider_album).await, 0);
    assert_eq!(
        credit_artist_at(&pool, library_track, 1).await,
        Some(identified)
    );
    assert_eq!(artist_source_owner(&pool, "bea-provider").await, identified);
}

#[sqlx::test]
async fn colliding_albums_merge_into_the_identified_artists_row(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let identified = Uuid::now_v7();
    let id_less = Uuid::now_v7();
    insert_artist(&pool, identified, "Shared Name", Some("mb-shared")).await;
    insert_artist(&pool, id_less, "Shared Name", None).await;
    insert_artist_source(&pool, id_less, "shared-provider").await;

    let library_track = Uuid::now_v7();
    insert_track(&pool, library_track, "Library Song", 180_000).await;
    insert_credit(&pool, library_track, 0, "Shared Name", identified, None).await;
    insert_credit(
        &pool,
        library_track,
        1,
        "Shared Name",
        id_less,
        Some("composer"),
    )
    .await;
    insert_track_source(
        &pool,
        library_track,
        "local",
        "named:/music",
        "library-song.flac",
    )
    .await;

    let provider_track = Uuid::now_v7();
    insert_track(&pool, provider_track, "Provider Song", 200_000).await;
    insert_credit(&pool, provider_track, 0, "Shared Name", id_less, None).await;

    let target_album = Uuid::now_v7();
    insert_album(&pool, target_album, "Switchblade", identified, None).await;
    seed_album_source(&pool, target_album, "target-album").await;
    insert_album_track(&pool, target_album, library_track, 0).await;

    let source_album = Uuid::now_v7();
    insert_album(&pool, source_album, "Switchblade", id_less, None).await;
    seed_album_source(&pool, source_album, "source-album").await;
    insert_album_track(&pool, source_album, library_track, 0).await;
    insert_album_track(&pool, source_album, provider_track, 1).await;

    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    insert_user(&pool, first, "first").await;
    insert_user(&pool, second, "second").await;
    let source_starred_at = "2026-01-01T00:00:00Z".parse().unwrap();
    let target_starred_at = "2026-01-02T00:00:00Z".parse().unwrap();
    let moved_starred_at = "2026-03-03T00:00:00Z".parse().unwrap();
    insert_artist_favorite(&pool, first, id_less, source_starred_at).await;
    insert_artist_favorite(&pool, first, identified, target_starred_at).await;
    insert_artist_favorite(&pool, second, id_less, moved_starred_at).await;
    insert_artist_rating(&pool, first, id_less, 5).await;
    insert_artist_rating(&pool, first, identified, 3).await;
    insert_artist_rating(&pool, second, id_less, 4).await;
    insert_album_favorite(&pool, first, source_album, source_starred_at).await;
    insert_album_favorite(&pool, first, target_album, target_starred_at).await;
    insert_album_favorite(&pool, second, source_album, moved_starred_at).await;
    insert_album_rating(&pool, first, source_album, 5).await;
    insert_album_rating(&pool, first, target_album, 3).await;
    insert_album_rating(&pool, second, source_album, 4).await;

    let before = snapshot_tables(&pool).await;
    let preview = repository.provider_duplicates(&[]).await.unwrap();
    assert_eq!(
        snapshot_tables(&pool).await,
        before,
        "the dry run rolls every table back"
    );

    let report = repository.delete_provider_duplicates(&[]).await.unwrap();
    assert_eq!(report.merges, preview.merges);
    assert_eq!(report.track_merges, preview.track_merges);
    assert_eq!(report.refetch, preview.refetch);
    assert_eq!(report.duplicates, preview.duplicates);
    assert_eq!(report.skipped, preview.skipped);
    assert_eq!(report.splits.len(), preview.splits.len());
    assert_eq!(report.splits[0].external_id, "source-album");

    assert_eq!(report.merges.len(), 1);
    assert_eq!(report.merges[0].credits, 2);
    assert_eq!(report.merges[0].albums_moved, 0);
    assert_eq!(report.merges[0].albums_merged, 1);
    assert_eq!(row_count(&pool, "artists", "id", id_less).await, 0);
    assert_eq!(row_count(&pool, "albums", "id", source_album).await, 0);
    assert_eq!(album_artist_of(&pool, target_album).await, Some(identified));
    let members = album_members(&pool, target_album).await;
    assert_eq!(members.len(), 2, "each membership is held once");
    assert!(members.contains(&library_track));
    assert!(members.contains(&provider_track));
    assert_eq!(
        credit_artist_at(&pool, library_track, 1).await,
        Some(identified)
    );
    assert_eq!(
        artist_source_owner(&pool, "shared-provider").await,
        identified
    );
    assert_eq!(
        row_count(&pool, "album_sources", "album_id", source_album).await,
        0
    );
    assert_eq!(
        album_source_owners(&pool, target_album).await,
        vec!["target-album".to_owned()]
    );
    assert_eq!(
        album_source_owners(&pool, report.splits[0].into).await,
        vec!["source-album".to_owned()]
    );

    assert_eq!(
        row_count(&pool, "artist_favorites", "artist_id", id_less).await,
        0
    );
    assert_eq!(
        row_count(&pool, "artist_ratings", "artist_id", id_less).await,
        0
    );
    assert_eq!(
        row_count(&pool, "artist_favorites", "artist_id", identified).await,
        2
    );
    assert_eq!(
        row_count(&pool, "artist_ratings", "artist_id", identified).await,
        2
    );
    let kept_artist_rating: i16 = sqlx::query_scalar(
        "SELECT rating FROM artist_ratings WHERE user_id = $1 AND artist_id = $2",
    )
    .bind(first)
    .bind(identified)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept_artist_rating, 3, "the identified artist's rating wins");
    let moved_artist_rating: i16 = sqlx::query_scalar(
        "SELECT rating FROM artist_ratings WHERE user_id = $1 AND artist_id = $2",
    )
    .bind(second)
    .bind(identified)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(moved_artist_rating, 4, "the moved rating keeps its value");
    assert_eq!(
        favorite_starred_at(&pool, "artist_favorites", "artist_id", first, identified).await,
        target_starred_at
    );
    assert_eq!(
        favorite_starred_at(&pool, "artist_favorites", "artist_id", second, identified).await,
        moved_starred_at,
        "the moved favourite keeps its timestamp"
    );

    assert_eq!(
        row_count(&pool, "album_favorites", "album_id", source_album).await,
        0
    );
    assert_eq!(
        row_count(&pool, "album_ratings", "album_id", source_album).await,
        0
    );
    assert_eq!(
        row_count(&pool, "album_favorites", "album_id", target_album).await,
        2
    );
    assert_eq!(
        row_count(&pool, "album_ratings", "album_id", target_album).await,
        2
    );
    let kept_album_rating: i16 =
        sqlx::query_scalar("SELECT rating FROM album_ratings WHERE user_id = $1 AND album_id = $2")
            .bind(first)
            .bind(target_album)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(kept_album_rating, 3, "the identified album's rating wins");
    let moved_album_rating: i16 =
        sqlx::query_scalar("SELECT rating FROM album_ratings WHERE user_id = $1 AND album_id = $2")
            .bind(second)
            .bind(target_album)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(moved_album_rating, 4, "the moved rating keeps its value");
    assert_eq!(
        favorite_starred_at(&pool, "album_favorites", "album_id", first, target_album).await,
        target_starred_at
    );
    assert_eq!(
        favorite_starred_at(&pool, "album_favorites", "album_id", second, target_album).await,
        moved_starred_at,
        "the moved favourite keeps its timestamp"
    );
}

#[sqlx::test]
async fn two_identified_namesakes_leave_a_credited_name_only_artist_alone(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_identified = Uuid::now_v7();
    let second_identified = Uuid::now_v7();
    let id_less = Uuid::now_v7();
    insert_artist(&pool, first_identified, "Shared Name", Some("mb-one")).await;
    insert_artist(&pool, second_identified, "Shared Name", Some("mb-two")).await;
    insert_artist(&pool, id_less, "Shared Name", None).await;

    let library_track = Uuid::now_v7();
    insert_track(&pool, library_track, "Library Song", 180_000).await;
    insert_credit(
        &pool,
        library_track,
        0,
        "Shared Name",
        first_identified,
        None,
    )
    .await;
    insert_credit(
        &pool,
        library_track,
        1,
        "Shared Name",
        id_less,
        Some("composer"),
    )
    .await;
    insert_track_source(
        &pool,
        library_track,
        "local",
        "named:/music",
        "library-song.flac",
    )
    .await;

    let report = repository.delete_provider_duplicates(&[]).await.unwrap();

    assert!(report.merges.is_empty());
    assert_eq!(row_count(&pool, "artists", "id", id_less).await, 1);
    assert_eq!(row_count(&pool, "artists", "id", first_identified).await, 1);
    assert_eq!(
        row_count(&pool, "artists", "id", second_identified).await,
        1
    );
    assert_eq!(
        credit_artist_at(&pool, library_track, 1).await,
        Some(id_less)
    );
}
