use std::sync::Arc;

use sqlx::PgPool;

use super::PostgresTrackRepository;
use super::schema::folded;
use crate::features::music_library::catalogue::LibraryService;
use crate::features::music_library::domain::{ArtistCredit, ArtistId};
use crate::features::music_library::storage::contracts::{
    CatalogueWrite, LibraryRead, SourceLookup, StateStore,
};

fn credit(name: &str, mbid: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), mbid.map(str::to_owned)).unwrap()
}

async fn named(repository: &PostgresTrackRepository, name: &str, mbid: Option<&str>) -> ArtistId {
    repository
        .resolve_or_create_artist(&credit(name, mbid))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn stored_under(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM artists WHERE name_normalized = {}",
        folded("$1")
    ))
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn identified(pool: &PgPool, name: &str) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM artists WHERE musicbrainz_artist_id IS NOT NULL \
         AND name_normalized = {}",
        folded("$1")
    ))
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn one_normalised_name_resolves_to_its_artist(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let alpha = named(&repository, "Alpha", None).await;
    let beta = named(&repository, "Beta", None).await;

    assert_eq!(
        repository.find_artist_by_exact_name("Alpha").await.unwrap(),
        Some(alpha.clone())
    );
    assert_eq!(
        repository.find_artist_by_exact_name("Beta").await.unwrap(),
        Some(beta.clone())
    );
    assert_ne!(
        repository.find_artist_by_exact_name("Alpha").await.unwrap(),
        Some(beta)
    );
}

#[sqlx::test]
async fn a_folded_variant_resolves_to_the_same_artist(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = named(&repository, "Placeholder", None).await;

    sqlx::query("UPDATE artists SET name = $2 WHERE id = $1")
        .bind(artist.as_uuid())
        .bind("It\u{2019}s   Just Forever")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        stored_under(&pool, "IT'S   JUST forever").await,
        1,
        "the fixture stores one row under the folded key"
    );
    assert_eq!(
        repository
            .find_artist_by_exact_name("IT'S   JUST forever")
            .await
            .unwrap(),
        Some(artist.clone()),
        "the read folds the bound name the same way the stored column was folded"
    );
    assert_eq!(
        repository
            .find_artist_by_exact_name("It\u{2019}s   Just Forever")
            .await
            .unwrap(),
        Some(artist)
    );
}

#[sqlx::test]
async fn an_absent_name_has_no_match(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    named(&repository, "Alpha", None).await;

    assert_eq!(
        repository.find_artist_by_exact_name("Gamma").await.unwrap(),
        None
    );
    assert_eq!(
        repository.find_artist_by_exact_name("").await.unwrap(),
        None
    );
}

#[sqlx::test]
async fn several_artists_under_one_normalised_name_are_not_picked(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = named(&repository, "Twin", Some("mbid-twin-first")).await;
    let second = named(&repository, "Twin", Some("mbid-twin-second")).await;

    assert_ne!(first, second, "the fixture created two distinct artists");
    assert_eq!(
        stored_under(&pool, "Twin").await,
        2,
        "the partial unique index permits two identified artists under one normalised name"
    );
    assert_eq!(
        identified(&pool, "Twin").await,
        2,
        "both rows carry a musicbrainz id, so neither is the null-identified pair"
    );

    assert_eq!(
        repository.find_artist_by_exact_name("Twin").await.unwrap(),
        None,
        "two matches are not resolved to one"
    );
    assert_eq!(
        repository.find_artist_by_exact_name("twin").await.unwrap(),
        None,
        "and the folded comparison still sees both"
    );
}

#[sqlx::test]
async fn a_prefix_or_contains_name_has_no_match(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = named(&repository, "Alphabet", None).await;

    assert_eq!(
        repository
            .find_artist_by_exact_name("Alphabet")
            .await
            .unwrap(),
        Some(artist)
    );

    for query in ["Alpha", "phabet", "Alphab", "Alphabet ", " Alphabet"] {
        assert_eq!(
            repository.find_artist_by_exact_name(query).await.unwrap(),
            None,
            "{query} is not the stored normalised name"
        );
    }
}

#[sqlx::test]
async fn the_service_collapses_absent_and_ambiguous_names_into_no_artist(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let service = LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        Arc::clone(&repository) as Arc<dyn StateStore>,
    );
    let solo = named(&repository, "Solo", None).await;

    assert_eq!(
        service.artist_by_exact_name("Solo").await.unwrap(),
        Some(solo)
    );
    assert_eq!(service.artist_by_exact_name("Nobody").await.unwrap(), None);
    assert_eq!(service.artist_by_exact_name("Sol").await.unwrap(), None);

    named(&repository, "Twin", Some("mbid-service-first")).await;
    named(&repository, "Twin", Some("mbid-service-second")).await;

    assert_eq!(
        service.artist_by_exact_name("Twin").await.unwrap(),
        None,
        "the public read reports no artist for several matches too"
    );
}
