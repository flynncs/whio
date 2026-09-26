use sqlx::PgPool;

use super::PostgresTrackRepository;
use crate::features::music_library::domain::{AlbumMetadata, ArtistCredit};
use crate::features::music_library::storage::contracts::CatalogueWrite;

fn credit(name: &str) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), None).unwrap()
}

fn album(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
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
