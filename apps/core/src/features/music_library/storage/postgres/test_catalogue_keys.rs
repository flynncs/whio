use sqlx::PgPool;

use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    AlbumMetadata, ArtistCredit, ProviderId, SourceIdentity, SourceScope, TrackMetadata,
};
use crate::features::music_library::storage::contracts::{CatalogueWrite, LibraryRead};

fn credit(name: &str) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), None).unwrap()
}

fn album(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(
        title.to_owned(),
        vec!["Test Artist".to_owned()],
        Some(240_000),
    )
    .unwrap()
}

async fn artist_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM artists")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn album_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM albums")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn track_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM tracks")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]

async fn resolving_a_typographic_name_finds_the_stored_row(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let stored = repository
        .resolve_or_create_artist(&credit("Don\u{2019}t Stop"))
        .await
        .unwrap();
    let before = artist_count(&pool).await;

    let again = repository
        .resolve_or_create_artist(&credit("Don\u{2019}t Stop"))
        .await
        .unwrap();
    let after = artist_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(stored.id(), again.id());
}

#[sqlx::test]

async fn resolving_a_typographic_title_finds_the_stored_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = credit("Cage the Elephant");

    let apostrophe = repository
        .resolve_or_create_album(
            &album("It\u{2019}s Just Forever").with_album_artist(artist.clone()),
        )
        .await
        .unwrap();
    let apostrophe_again = repository
        .resolve_or_create_album(
            &album("It\u{2019}s Just Forever").with_album_artist(artist.clone()),
        )
        .await
        .unwrap();

    let en_dash = repository
        .resolve_or_create_album(&album("Sun \u{2013} Ra").with_album_artist(artist.clone()))
        .await
        .unwrap();
    let non_breaking_hyphen = repository
        .resolve_or_create_album(&album("Sun \u{2011} Ra").with_album_artist(artist))
        .await
        .unwrap();

    assert_eq!(apostrophe.id(), apostrophe_again.id());
    assert_eq!(en_dash.id(), non_breaking_hyphen.id());
    assert_eq!(album_count(&pool).await, 2);
}

#[sqlx::test]

async fn a_plain_name_still_resolves(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = credit("Daft Punk");

    let stored = repository.resolve_or_create_artist(&artist).await.unwrap();
    let recased = repository
        .resolve_or_create_artist(&credit("DAFT PUNK"))
        .await
        .unwrap();
    let again = repository.resolve_or_create_artist(&artist).await.unwrap();

    assert_eq!(stored.id(), recased.id());
    assert_eq!(stored.id(), again.id());
    assert_eq!(artist_count(&pool).await, 1);

    let album_metadata = album("Discovery").with_album_artist(artist);
    let stored_album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();
    let again_album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();

    assert_eq!(stored_album.id(), again_album.id());
    assert_eq!(album_count(&pool).await, 1);
}

#[sqlx::test]

async fn the_two_sites_fold_the_same_way(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let shared = "Don\u{2011}t Stop";
    let artist_credit = credit(shared);

    let stored_artist = repository
        .resolve_or_create_artist(&artist_credit)
        .await
        .unwrap();
    let stored_album = repository
        .resolve_or_create_album(&album(shared).with_album_artist(artist_credit.clone()))
        .await
        .unwrap();

    let artist_again = repository
        .resolve_or_create_artist(&artist_credit)
        .await
        .unwrap();
    let album_again = repository
        .resolve_or_create_album(&album(shared).with_album_artist(artist_credit))
        .await
        .unwrap();

    assert_eq!(stored_artist.id(), artist_again.id());
    assert_eq!(stored_album.id(), album_again.id());
    assert_eq!(artist_count(&pool).await, 1);
    assert_eq!(album_count(&pool).await, 1);
}

#[sqlx::test]

async fn a_non_breaking_space_artist_name_finds_the_stored_row(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let name = "Daft\u{a0}Punk";

    let stored = repository
        .resolve_or_create_artist(&credit(name))
        .await
        .unwrap();
    let before = artist_count(&pool).await;

    let again = repository
        .resolve_or_create_artist(&credit(name))
        .await
        .unwrap();
    let after = artist_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(stored.id(), again.id());
}

#[sqlx::test]

async fn a_non_breaking_space_album_title_finds_the_stored_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = credit("Cage the Elephant");
    let title = "Sun\u{a0}Ra";

    let stored = repository
        .resolve_or_create_album(&album(title).with_album_artist(artist.clone()))
        .await
        .unwrap();
    let before = album_count(&pool).await;

    let again = repository
        .resolve_or_create_album(&album(title).with_album_artist(artist))
        .await
        .unwrap();
    let after = album_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(stored.id(), again.id());
}

#[sqlx::test]

async fn a_dotted_capital_artist_name_finds_the_stored_row(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let name = "\u{130}stanbul";

    let stored = repository
        .resolve_or_create_artist(&credit(name))
        .await
        .unwrap();
    let before = artist_count(&pool).await;

    let again = repository
        .resolve_or_create_artist(&credit(name))
        .await
        .unwrap();
    let after = artist_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(stored.id(), again.id());
}

#[sqlx::test]

async fn a_dotted_capital_album_title_finds_the_stored_album(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = credit("Cage the Elephant");
    let title = "\u{130}stanbul Sessions";

    let stored = repository
        .resolve_or_create_album(&album(title).with_album_artist(artist.clone()))
        .await
        .unwrap();
    let before = album_count(&pool).await;

    let again = repository
        .resolve_or_create_album(&album(title).with_album_artist(artist))
        .await
        .unwrap();
    let after = album_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(stored.id(), again.id());
}

#[sqlx::test]

async fn a_non_breaking_space_title_is_a_candidate(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let title = "Song\u{a0}One";
    let stored = repository
        .get_or_create(source("candidate-nbsp"), metadata(title))
        .await
        .unwrap();
    let before = track_count(&pool).await;

    let first = repository
        .candidate_tracks(&[title.to_owned()], 25)
        .await
        .unwrap();
    let second = repository
        .candidate_tracks(&[title.to_owned()], 25)
        .await
        .unwrap();
    let after = track_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].track().id(), stored.id());
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].track().id(), stored.id());
}

#[sqlx::test]

async fn a_dotted_capital_title_is_a_candidate(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let title = "\u{130}\u{130}\u{130}\u{130}\u{130}";
    let stored = repository
        .get_or_create(source("candidate-dotted"), metadata(title))
        .await
        .unwrap();
    let before = track_count(&pool).await;

    let deduped = repository
        .dedup_candidates(&[title.to_owned()], 25)
        .await
        .unwrap();
    let candidates = repository
        .candidate_tracks(&[title.to_owned()], 25)
        .await
        .unwrap();
    let after = track_count(&pool).await;

    assert_eq!(before, 1);
    assert_eq!(after, 1);
    assert_eq!(deduped.len(), 1);
    assert_eq!(deduped[0].track().id(), stored.id());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].track().id(), stored.id());
}
