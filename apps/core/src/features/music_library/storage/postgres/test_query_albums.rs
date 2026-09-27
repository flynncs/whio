use std::collections::{BTreeMap, BTreeSet};

use sqlx::PgPool;
use uuid::Uuid;

use super::PostgresTrackRepository;
use crate::features::identity::provision;
use crate::features::music_library::{
    domain::{
        AlbumId, AlbumListFilter, AlbumListScope, AlbumMetadata, AlbumOrder, AlbumTrack,
        ArtistCredit, ArtistId, CreditRole, IdSet, ProviderId, RandomSeed, Range, RoleSelector,
        RoleSet, SelectorText, SortDirection, SourceIdentity, SourceScope, StateTarget, TrackId,
        TrackMetadata, ValidationError,
    },
    library::LibraryId,
    library_state::test_support::seed_play,
    storage::contracts::{CatalogueWrite, LibraryRead, StateStore, TrackRepositoryError},
};

const LOCAL_ROOT: &str = "/music";
const OTHER_ROOT: &str = "/elsewhere";

fn source(provider: &str, external_id: &str) -> SourceIdentity {
    source_in(provider, LOCAL_ROOT, external_id)
}

fn source_in(provider: &str, root: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider.to_owned()).unwrap(),
        if provider == "local" {
            SourceScope::Named(root.to_owned())
        } else {
            SourceScope::Global
        },
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata_by(title: &str, primary: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec![primary.to_owned()], Some(1_000)).unwrap()
}

fn credit(name: &str, role: Option<CreditRole>) -> ArtistCredit {
    let credit = ArtistCredit::new(name.to_owned(), None).unwrap();

    match role {
        Some(role) => credit.with_role(role),
        None => credit,
    }
}

fn album(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
}

fn artist_id_set(ids: &[&ArtistId]) -> IdSet<ArtistId> {
    IdSet::new(ids.iter().map(|id| (*id).clone())).unwrap()
}

fn selector(value: &str) -> Option<SelectorText> {
    SelectorText::new(value).unwrap()
}

fn text_set(values: &[&str]) -> IdSet<SelectorText> {
    IdSet::new(values.iter().map(|value| selector(value).unwrap())).unwrap()
}

fn role_set(values: &[RoleSelector]) -> RoleSet {
    RoleSet::new(values.to_vec()).unwrap()
}

fn range<T: Ord + Copy>(from: Option<T>, to: Option<T>) -> Range<T> {
    Range::new(from, to).unwrap()
}

fn id_set(ids: &[&AlbumId]) -> BTreeSet<Uuid> {
    ids.iter().map(|id| *id.as_uuid()).collect()
}

fn sorted(ids: &[&AlbumId]) -> Vec<AlbumId> {
    let mut ids = ids.iter().map(|id| (*id).clone()).collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    ids
}

async fn listed(
    repository: &PostgresTrackRepository,
    order: AlbumOrder,
    filter: &AlbumListFilter,
    user: Option<&Uuid>,
) -> Vec<AlbumId> {
    listed_at(repository, 0, 500, order, filter, user).await
}

async fn listed_at(
    repository: &PostgresTrackRepository,
    offset: u32,
    limit: u32,
    order: AlbumOrder,
    filter: &AlbumListFilter,
    user: Option<&Uuid>,
) -> Vec<AlbumId> {
    repository
        .list_albums(offset, limit, order, filter.clone(), user.copied())
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.album().id().clone())
        .collect()
}

async fn total(
    repository: &PostgresTrackRepository,
    filter: &AlbumListFilter,
    user: Option<&Uuid>,
) -> u32 {
    repository
        .count_albums(user.copied(), filter.clone())
        .await
        .unwrap()
}

async fn credit_artist(pool: &PgPool, track: &TrackId, name: &str) -> ArtistId {
    let id: Uuid = sqlx::query_scalar(
        "SELECT artist_id FROM track_artist_credits \
         WHERE track_id = $1 AND name = $2 ORDER BY position LIMIT 1",
    )
    .bind(track.as_uuid())
    .bind(name)
    .fetch_one(pool)
    .await
    .unwrap();

    ArtistId::from_uuid(id)
}

async fn album_artist_of(pool: &PgPool, album: &AlbumId) -> ArtistId {
    let id: Uuid = sqlx::query_scalar("SELECT album_artist_id FROM albums WHERE id = $1")
        .bind(album.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap();

    ArtistId::from_uuid(id)
}

fn encoded(source: &SourceIdentity) -> String {
    match source.source_scope() {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(root) => format!("named:{root}"),
    }
}

async fn set_missing(pool: &PgPool, source: &SourceIdentity, missing: bool) {
    sqlx::query(
        "UPDATE track_sources SET missing_at = CASE WHEN $4 THEN CURRENT_TIMESTAMP ELSE NULL END \
         WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3",
    )
    .bind(source.provider_id().as_str())
    .bind(encoded(source))
    .bind(source.external_id())
    .bind(missing)
    .execute(pool)
    .await
    .unwrap();
}

async fn age_album(pool: &PgPool, album: &AlbumId, days: i64) {
    sqlx::query("UPDATE albums SET created_at = $1 WHERE id = $2")
        .bind(chrono::Utc::now() + chrono::Duration::days(days))
        .bind(album.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

async fn age_album_source(pool: &PgPool, album: &AlbumId, days: i64) {
    sqlx::query("UPDATE album_sources SET created_at = $1 WHERE album_id = $2")
        .bind(chrono::Utc::now() + chrono::Duration::days(days))
        .bind(album.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

async fn age_album_favorite(pool: &PgPool, album: &AlbumId, user: Uuid, days: i64) {
    sqlx::query("UPDATE album_favorites SET starred_at = $3 WHERE album_id = $1 AND user_id = $2")
        .bind(album.as_uuid())
        .bind(user)
        .bind(chrono::Utc::now() + chrono::Duration::days(days))
        .execute(pool)
        .await
        .unwrap();
}

async fn register_library(pool: &PgPool, label: &str, root: &str) -> LibraryId {
    let id = Uuid::now_v7();

    sqlx::query("INSERT INTO libraries (id, label) VALUES ($1, $2)")
        .bind(id)
        .bind(label)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO library_roots (library_id, root) VALUES ($1, $2)")
        .bind(id)
        .bind(root)
        .execute(pool)
        .await
        .unwrap();

    LibraryId::from_uuid(id)
}

async fn place(repository: &PostgresTrackRepository, album: &AlbumId, track: &TrackId) {
    repository
        .set_album_track(album, track, &AlbumTrack::unplaced())
        .await
        .unwrap();
}

struct Fixture {
    kept: AlbumId,
    other: AlbumId,
    third: AlbumId,
    remote: AlbumId,
    other_track: TrackId,
    kept_artist: ArtistId,
    composer: ArtistId,
    beta_writer: ArtistId,
}

async fn album_fixture(repository: &PostgresTrackRepository, pool: &PgPool) -> Fixture {
    let kept = repository
        .resolve_or_create_album(
            &album("Kept")
                .with_album_artist(credit("Album Artist", None))
                .with_year(Some(1999))
                .unwrap()
                .with_compilation(Some(false)),
        )
        .await
        .unwrap();
    let other = repository
        .resolve_or_create_album(&album("Other").with_year(Some(2020)).unwrap())
        .await
        .unwrap();
    let third = repository
        .resolve_or_create_album(
            &album("Third")
                .with_album_artist(credit("Third Artist", None))
                .with_compilation(Some(true)),
        )
        .await
        .unwrap();
    let remote = repository
        .resolve_or_create_album(&album("Remote"))
        .await
        .unwrap();

    let alpha = repository
        .get_or_create(
            source("local", "alpha.flac"),
            metadata_by("alpha", "Alpha Voice")
                .with_genres(vec!["Rock".to_owned(), "Pop".to_owned()])
                .with_credits(vec![credit("Composer One", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    let beta = repository
        .get_or_create(
            source_in("local", OTHER_ROOT, "beta.flac"),
            metadata_by("beta", "Beta Voice")
                .with_genres(vec!["Jazz".to_owned()])
                .with_credits(vec![credit("Beta Writer", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    let delta = repository
        .get_or_create(
            source("local", "delta.flac"),
            metadata_by("delta", "Delta Voice"),
        )
        .await
        .unwrap();
    let remote_track = repository
        .get_or_create(
            source("youtube_music", "vid-remote"),
            metadata_by("remote", "Remote Voice"),
        )
        .await
        .unwrap();

    for (album_row, track) in [
        (&kept, &alpha),
        (&other, &beta),
        (&third, &delta),
        (&remote, &remote_track),
    ] {
        place(repository, album_row.id(), track.id()).await;
    }

    for (album_row, external) in [
        (&kept, "kept.flac"),
        (&other, "other.flac"),
        (&third, "third.flac"),
    ] {
        let root = if album_row.id() == other.id() {
            OTHER_ROOT
        } else {
            LOCAL_ROOT
        };

        repository
            .set_album_source(album_row.id(), &source_in("local", root, external))
            .await
            .unwrap();
    }

    age_album_source(pool, kept.id(), -10).await;
    age_album_source(pool, other.id(), -5).await;
    age_album_source(pool, third.id(), -1).await;

    Fixture {
        kept: kept.id().clone(),
        other: other.id().clone(),
        third: third.id().clone(),
        remote: remote.id().clone(),
        other_track: beta.id().clone(),
        kept_artist: album_artist_of(pool, kept.id()).await,
        composer: credit_artist(pool, alpha.id(), "Composer One").await,
        beta_writer: credit_artist(pool, beta.id(), "Beta Writer").await,
    }
}

#[sqlx::test]
async fn count_equals_the_list_for_every_selector(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("album-selector", "Album Selector")
        .await
        .unwrap();
    let fixture = album_fixture(&repository, &pool).await;

    repository
        .set_starred(user, StateTarget::Album(&fixture.kept), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Album(&fixture.kept), Some(4))
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Album(&fixture.remote), true)
        .await
        .unwrap();
    seed_play(&pool, user, &fixture.other_track, chrono::Utc::now()).await;
    set_missing(&pool, &source_in("local", OTHER_ROOT, "beta.flac"), true).await;

    let music = register_library(&pool, "main", LOCAL_ROOT).await;
    let elsewhere = register_library(&pool, "elsewhere", OTHER_ROOT).await;
    let since = chrono::Utc::now() - chrono::Duration::days(7);
    let widened = chrono::Utc::now() - chrono::Duration::days(30);

    let fixtures: Vec<(&str, AlbumListFilter, Vec<&AlbumId>)> = vec![
        (
            "name",
            AlbumListFilter::new().with_name(selector("kept")),
            vec![&fixture.kept],
        ),
        (
            "blank name",
            AlbumListFilter::new().with_name(selector("   ")),
            vec![
                &fixture.kept,
                &fixture.other,
                &fixture.third,
                &fixture.remote,
            ],
        ),
        (
            "artist_id",
            AlbumListFilter::new().with_artist_id(Some(artist_id_set(&[&fixture.kept_artist]))),
            vec![&fixture.kept],
        ),
        (
            "album_artist_id",
            AlbumListFilter::new()
                .with_album_artist_id(Some(artist_id_set(&[&fixture.kept_artist]))),
            vec![&fixture.kept],
        ),
        (
            "artists_id",
            AlbumListFilter::new().with_artists_id(Some(artist_id_set(&[&fixture.beta_writer]))),
            vec![&fixture.other],
        ),
        (
            "artists_id composer",
            AlbumListFilter::new().with_artists_id(Some(artist_id_set(&[&fixture.composer]))),
            vec![&fixture.kept],
        ),
        (
            "role",
            AlbumListFilter::new().with_role(Some(role_set(&[RoleSelector::Composer]))),
            vec![&fixture.kept, &fixture.other],
        ),
        (
            "genre",
            AlbumListFilter::new().with_genre(Some(text_set(&["rock"]))),
            vec![&fixture.kept],
        ),
        (
            "year",
            AlbumListFilter::new().with_year(Some(range(Some(1990), Some(2010)))),
            vec![&fixture.kept],
        ),
        (
            "compilation",
            AlbumListFilter::new().with_compilation(Some(false)),
            vec![&fixture.kept],
        ),
        (
            "starred",
            AlbumListFilter::new().with_starred(Some(true)),
            vec![&fixture.kept, &fixture.remote],
        ),
        (
            "has_rating",
            AlbumListFilter::new().with_has_rating(Some(true)),
            vec![&fixture.kept],
        ),
        (
            "has_rating false",
            AlbumListFilter::new().with_has_rating(Some(false)),
            vec![&fixture.other, &fixture.third, &fixture.remote],
        ),
        (
            "rating",
            AlbumListFilter::new().with_rating(Some(range(Some(4), Some(5)))),
            vec![&fixture.kept],
        ),
        (
            "recently_played",
            AlbumListFilter::new().with_recently_played(Some(true)),
            vec![&fixture.other],
        ),
        (
            "recently_played false",
            AlbumListFilter::new().with_recently_played(Some(false)),
            vec![&fixture.kept, &fixture.third, &fixture.remote],
        ),
        (
            "play_count",
            AlbumListFilter::new().with_play_count(Some(range(Some(1), None))),
            vec![&fixture.other],
        ),
        (
            "play_count zero",
            AlbumListFilter::new().with_play_count(Some(range(Some(0), Some(0)))),
            vec![&fixture.kept, &fixture.third, &fixture.remote],
        ),
        (
            "date_added",
            AlbumListFilter::new().with_date_added(Some(range(Some(since), None))),
            vec![&fixture.other, &fixture.third],
        ),
        (
            "missing",
            AlbumListFilter::new().with_missing(Some(true)),
            vec![&fixture.other],
        ),
        (
            "missing false",
            AlbumListFilter::new().with_missing(Some(false)),
            vec![&fixture.kept, &fixture.third, &fixture.remote],
        ),
        (
            "library_id",
            AlbumListFilter::new().with_library_id(Some(IdSet::new([music.clone()]).unwrap())),
            vec![&fixture.kept, &fixture.third],
        ),
        (
            "library_id elsewhere",
            AlbumListFilter::new().with_library_id(Some(IdSet::new([elsewhere]).unwrap())),
            vec![&fixture.other],
        ),
    ];

    for (name, filter, expected) in fixtures {
        for scope in [AlbumListScope::Engaged, AlbumListScope::Owned] {
            let expected = if scope == AlbumListScope::Owned {
                expected
                    .iter()
                    .filter(|id| **id != &fixture.remote)
                    .copied()
                    .collect::<Vec<_>>()
            } else {
                expected.clone()
            };
            let filter = filter.clone().with_scope(scope);
            let observed = listed(&repository, AlbumOrder::Name, &filter, Some(&user)).await;
            let observed_total = total(&repository, &filter, Some(&user)).await;

            assert_eq!(
                observed
                    .iter()
                    .map(|id| *id.as_uuid())
                    .collect::<BTreeSet<_>>(),
                id_set(&expected),
                "the {name} selector under {scope:?}"
            );
            assert_eq!(
                observed_total,
                u32::try_from(expected.len()).unwrap(),
                "the {name} count under {scope:?}"
            );
        }
    }

    let all = AlbumListFilter::new()
        .with_name(selector("kept"))
        .with_artist_id(Some(artist_id_set(&[&fixture.kept_artist])))
        .with_album_artist_id(Some(artist_id_set(&[&fixture.kept_artist])))
        .with_artists_id(Some(artist_id_set(&[&fixture.composer])))
        .with_role(Some(role_set(&[RoleSelector::Composer])))
        .with_genre(Some(text_set(&["rock"])))
        .with_year(Some(range(Some(1990), Some(2010))))
        .with_compilation(Some(false))
        .with_starred(Some(true))
        .with_has_rating(Some(true))
        .with_rating(Some(range(Some(4), Some(5))))
        .with_play_count(Some(range(Some(0), Some(0))))
        .with_recently_played(Some(false))
        .with_date_added(Some(range(Some(widened), None)))
        .with_missing(Some(false))
        .with_library_id(Some(IdSet::new([music.clone()]).unwrap()));

    assert_eq!(
        listed(&repository, AlbumOrder::Name, &all, Some(&user)).await,
        vec![fixture.kept.clone()],
        "every selector at once is an AND"
    );
    assert_eq!(total(&repository, &all, Some(&user)).await, 1);

    let missing = LibraryId::from_uuid(Uuid::now_v7());
    let unknown =
        AlbumListFilter::new().with_library_id(Some(IdSet::new([missing.clone()]).unwrap()));
    assert!(
        matches!(
            repository
                .list_albums(0, 500, AlbumOrder::Name, unknown.clone(), Some(user))
                .await,
            Err(TrackRepositoryError::UnknownLibraryId { id }) if id == missing.to_string()
        ),
        "an unregistered library id is rejected, not ignored"
    );
    assert!(
        matches!(
            repository.count_albums(Some(user), unknown).await,
            Err(TrackRepositoryError::UnknownLibraryId { id }) if id == missing.to_string()
        ),
        "and the count rejects it too"
    );
}

#[sqlx::test]
async fn membership_terms_do_not_multiply_rows(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let doubled = repository
        .resolve_or_create_album(&album("Doubled"))
        .await
        .unwrap();
    let genred = repository
        .resolve_or_create_album(&album("Genred"))
        .await
        .unwrap();

    let mut credited_track = None;

    for external in ["one.flac", "two.flac"] {
        let track = repository
            .get_or_create(
                source("local", external),
                metadata_by("credited", "Doubled").with_credits(vec![
                    credit("Doubled", Some(CreditRole::Composer)),
                    credit("Doubled", Some(CreditRole::Performer)),
                ]),
            )
            .await
            .unwrap();
        place(&repository, doubled.id(), track.id()).await;
        credited_track.get_or_insert(track.id().clone());
    }

    for (external, genres) in [
        ("rock.flac", vec!["Rock".to_owned(), "Pop".to_owned()]),
        ("rock-again.flac", vec!["Rock".to_owned()]),
    ] {
        let track = repository
            .get_or_create(
                source("local", external),
                metadata_by("genred", "Genred").with_genres(genres),
            )
            .await
            .unwrap();
        place(&repository, genred.id(), track.id()).await;
    }

    let artist = credit_artist(&pool, &credited_track.unwrap(), "Doubled").await;
    let credited = AlbumListFilter::new().with_artists_id(Some(artist_id_set(&[&artist])));

    assert_eq!(
        listed(&repository, AlbumOrder::Name, &credited, None).await,
        vec![doubled.id().clone()],
        "an album whose two member tracks credit the artist appears once"
    );
    assert_eq!(total(&repository, &credited, None).await, 1);

    let both_genres = AlbumListFilter::new().with_genre(Some(text_set(&["rock", "pop"])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &both_genres, None).await,
        vec![genred.id().clone()],
        "two matching genres on one album appear once"
    );
    assert_eq!(total(&repository, &both_genres, None).await, 1);

    let single = AlbumListFilter::new().with_genre(Some(text_set(&["rock"])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &single, None).await,
        vec![genred.id().clone()]
    );
    assert_eq!(total(&repository, &single, None).await, 1);
}

#[sqlx::test]
async fn album_selectors_are_not_aliases(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let x = repository
        .resolve_or_create_artist(&credit("X", None))
        .await
        .unwrap()
        .id()
        .clone();

    let both = repository
        .resolve_or_create_album(&album("Both").with_album_artist(credit("X", None)))
        .await
        .unwrap();
    let album_side = repository
        .resolve_or_create_album(&album("AlbumSide").with_album_artist(credit("X", None)))
        .await
        .unwrap();
    let credit_side = repository
        .resolve_or_create_album(&album("CreditSide").with_album_artist(credit("W", None)))
        .await
        .unwrap();
    let neither = repository
        .resolve_or_create_album(&album("Neither").with_album_artist(credit("W", None)))
        .await
        .unwrap();

    for (album_row, primary) in [
        (&both, "X"),
        (&album_side, "Y"),
        (&credit_side, "X"),
        (&neither, "Y"),
    ] {
        let track = repository
            .get_or_create(
                source("local", &format!("{}.flac", album_row.title())),
                metadata_by("member", primary),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
    }

    let by_album_artist = AlbumListFilter::new().with_album_artist_id(Some(artist_id_set(&[&x])));
    let by_member_credit = AlbumListFilter::new().with_artists_id(Some(artist_id_set(&[&x])));
    let by_artist_id = AlbumListFilter::new().with_artist_id(Some(artist_id_set(&[&x])));

    let album_artist_set = vec![album_side.id().clone(), both.id().clone()];
    let member_credit_set = vec![both.id().clone(), credit_side.id().clone()];
    let artist_id_set = vec![
        album_side.id().clone(),
        both.id().clone(),
        credit_side.id().clone(),
    ];

    let observed_album_artist = listed(&repository, AlbumOrder::Name, &by_album_artist, None).await;
    let observed_member_credit =
        listed(&repository, AlbumOrder::Name, &by_member_credit, None).await;
    let observed_artist_id = listed(&repository, AlbumOrder::Name, &by_artist_id, None).await;

    assert_eq!(
        observed_album_artist, album_artist_set,
        "album_artist_id is album-artist membership, ordered by name"
    );
    assert_eq!(
        observed_member_credit, member_credit_set,
        "artists_id is member-track credit membership, ordered by name"
    );
    assert_eq!(
        observed_artist_id, artist_id_set,
        "artist_id is the documented superset of both, ordered by name"
    );
    assert_ne!(album_artist_set, member_credit_set);
    assert_ne!(artist_id_set, album_artist_set);
    assert_ne!(artist_id_set, member_credit_set);

    for (name, filter, expected) in [
        ("album_artist_id", by_album_artist, &album_artist_set),
        ("artists_id", by_member_credit, &member_credit_set),
        ("artist_id", by_artist_id, &artist_id_set),
    ] {
        assert_eq!(
            total(&repository, &filter, None).await,
            u32::try_from(expected.len()).unwrap(),
            "the {name} count"
        );
    }

    assert!(
        !artist_id_set.contains(neither.id()),
        "and Y matches nothing"
    );
    assert!(!album_artist_set.contains(credit_side.id()));
    assert!(!member_credit_set.contains(album_side.id()));
}

#[sqlx::test]
async fn genre_reads_the_set_not_the_projection(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album_row = repository
        .resolve_or_create_album(&album("Mixed"))
        .await
        .unwrap();
    let first = repository
        .get_or_create(
            source("local", "first.flac"),
            metadata_by("first", "Narrator")
                .with_genres(vec!["Rock".to_owned(), "Electronic".to_owned()]),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            source("local", "second.flac"),
            metadata_by("second", "Narrator").with_genres(vec!["Jazz".to_owned()]),
        )
        .await
        .unwrap();
    place(&repository, album_row.id(), first.id()).await;
    place(&repository, album_row.id(), second.id()).await;

    let projection: String = sqlx::query_scalar("SELECT genre FROM tracks WHERE id = $1")
        .bind(first.id().as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        projection, "Rock",
        "the display projection holds only the first value"
    );

    for name in ["rock", "electronic", "jazz"] {
        let filter = AlbumListFilter::new().with_genre(Some(text_set(&[name])));

        assert_eq!(
            listed(&repository, AlbumOrder::Name, &filter, None).await,
            vec![album_row.id().clone()],
            "the {name} set value finds the album"
        );
        assert_eq!(
            total(&repository, &filter, None).await,
            1,
            "the {name} count"
        );
    }

    let absent = AlbumListFilter::new().with_genre(Some(text_set(&["polka"])));
    assert!(
        listed(&repository, AlbumOrder::Name, &absent, None)
            .await
            .is_empty()
    );
    assert_eq!(total(&repository, &absent, None).await, 0);
}

#[sqlx::test]
async fn role_scoping_selects_the_right_membership(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let x = repository
        .resolve_or_create_artist(&credit("X", None))
        .await
        .unwrap()
        .id()
        .clone();
    let mut album_artist_albums = Vec::new();

    for title in ["XAlbumOne", "XAlbumTwo"] {
        let album_row = repository
            .resolve_or_create_album(&album(title).with_album_artist(credit("X", None)))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("{title}.flac")),
                metadata_by("member", "Y"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        album_artist_albums.push(album_row.id().clone());
    }

    let composed = repository
        .resolve_or_create_album(&album("XComposed").with_album_artist(credit("Z", None)))
        .await
        .unwrap();
    let composed_track = repository
        .get_or_create(
            source("local", "composed.flac"),
            metadata_by("member", "Y").with_credits(vec![credit("X", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    place(&repository, composed.id(), composed_track.id()).await;

    let primary = repository
        .resolve_or_create_album(&album("XPrimary").with_album_artist(credit("Z", None)))
        .await
        .unwrap();
    let primary_track = repository
        .get_or_create(source("local", "primary.flac"), metadata_by("member", "X"))
        .await
        .unwrap();
    place(&repository, primary.id(), primary_track.id()).await;

    let composed_filter = AlbumListFilter::new()
        .with_artists_id(Some(artist_id_set(&[&x])))
        .with_role(Some(role_set(&[RoleSelector::Composer])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &composed_filter, None).await,
        vec![composed.id().clone()],
        "artists_id with role=composer means that artist as a composer"
    );
    assert_eq!(total(&repository, &composed_filter, None).await, 1);

    let album_artist_filter = AlbumListFilter::new()
        .with_artists_id(Some(artist_id_set(&[&x])))
        .with_role(Some(role_set(&[RoleSelector::AlbumArtist])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &album_artist_filter, None).await,
        vec![
            album_artist_albums[0].clone(),
            album_artist_albums[1].clone()
        ],
        "artists_id with role=albumartist means album-artist membership"
    );
    assert_eq!(total(&repository, &album_artist_filter, None).await, 2);
    assert_ne!(
        total(&repository, &composed_filter, None).await,
        total(&repository, &album_artist_filter, None).await,
        "X being both a composer and an album artist gives two different counts"
    );

    let any_credit = AlbumListFilter::new().with_artists_id(Some(artist_id_set(&[&x])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &any_credit, None).await,
        vec![composed.id().clone(), primary.id().clone()],
        "without a role, artists_id is any member credit"
    );

    let composers_only =
        AlbumListFilter::new().with_role(Some(role_set(&[RoleSelector::Composer])));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &composers_only, None).await,
        vec![composed.id().clone()]
    );

    let album_artists_only =
        AlbumListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist])));
    assert_eq!(
        total(&repository, &album_artists_only, None).await,
        4,
        "every album with an album artist, whatever the artist"
    );

    let unknown = repository
        .resolve_or_create_artist(&credit("Unknown", None))
        .await
        .unwrap()
        .id()
        .clone();
    let absent = AlbumListFilter::new()
        .with_artists_id(Some(artist_id_set(&[&unknown])))
        .with_role(Some(role_set(&[RoleSelector::Composer])));
    assert!(
        listed(&repository, AlbumOrder::Name, &absent, None)
            .await
            .is_empty(),
        "an artist with no member credit in that role matches nothing"
    );
    assert_eq!(total(&repository, &absent, None).await, 0);
}

#[sqlx::test]
async fn rating_absent_last_in_both_directions(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("rating-sort", "Rating Sort")
        .await
        .unwrap();
    let mut rated = Vec::new();

    for index in 0..4u8 {
        let album_row = repository
            .resolve_or_create_album(&album(&format!("rated {index}")))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("rated-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;

        if index > 0 {
            repository
                .set_rating(user, StateTarget::Album(album_row.id()), Some(index))
                .await
                .unwrap();
        }

        rated.push(album_row.id().clone());
    }

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::Rating,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            Some(&user),
        )
        .await,
        vec![
            rated[1].clone(),
            rated[2].clone(),
            rated[3].clone(),
            rated[0].clone()
        ],
        "the unrated album sorts last in ascending order"
    );

    let descending = listed(
        &repository,
        AlbumOrder::Rating,
        &AlbumListFilter::new(),
        Some(&user),
    )
    .await;
    assert_eq!(
        descending,
        vec![
            rated[3].clone(),
            rated[2].clone(),
            rated[1].clone(),
            rated[0].clone()
        ],
        "and last in the natural descending order too"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::Rating,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Descending)),
            Some(&user),
        )
        .await,
        descending
    );
}

#[sqlx::test]
async fn starred_at_absent_last_in_both_directions(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("starred-sort", "Starred Sort")
        .await
        .unwrap();

    let mut starred = Vec::new();

    for index in 0..4u8 {
        let album_row = repository
            .resolve_or_create_album(&album(&format!("starred {index}")))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("starred-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;

        if index > 0 {
            repository
                .set_starred(user, StateTarget::Album(album_row.id()), true)
                .await
                .unwrap();
            age_album_favorite(&pool, album_row.id(), user, -i64::from(index)).await;
        }

        starred.push(album_row.id().clone());
    }

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::StarredAt,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            Some(&user),
        )
        .await,
        vec![
            starred[3].clone(),
            starred[2].clone(),
            starred[1].clone(),
            starred[0].clone()
        ],
        "starred_at ascending places the unstarred album last"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::StarredAt,
            &AlbumListFilter::new(),
            Some(&user),
        )
        .await,
        vec![
            starred[1].clone(),
            starred[2].clone(),
            starred[3].clone(),
            starred[0].clone()
        ],
        "starred_at descending places it last too"
    );
}

#[sqlx::test]
async fn max_year_absent_last_in_both_directions(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let mut years = Vec::new();

    for (index, year) in [Some(1990), Some(2000), Some(2010), None]
        .into_iter()
        .enumerate()
    {
        let metadata = match year {
            Some(year) => album(&format!("year {index}"))
                .with_year(Some(year))
                .unwrap(),
            None => album(&format!("year {index}")),
        };
        let album_row = repository.resolve_or_create_album(&metadata).await.unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("year-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        years.push(album_row.id().clone());
    }

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::MaxYear,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            None,
        )
        .await,
        vec![
            years[0].clone(),
            years[1].clone(),
            years[2].clone(),
            years[3].clone()
        ],
        "max_year ascending places the year-less album last"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::MaxYear,
            &AlbumListFilter::new(),
            None
        )
        .await,
        vec![
            years[2].clone(),
            years[1].clone(),
            years[0].clone(),
            years[3].clone()
        ],
        "and max_year descending places it last too"
    );
}

#[sqlx::test]
async fn equal_names_fall_back_to_the_canonical_id(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut tied = Vec::new();

    for index in 0..3u32 {
        let album_row = repository
            .resolve_or_create_album(
                &album("Shared").with_album_artist(credit(&format!("same {index}"), None)),
            )
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("shared-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        tied.push(album_row.id().clone());
    }

    assert_eq!(
        listed(&repository, AlbumOrder::Name, &AlbumListFilter::new(), None).await,
        sorted(&[&tied[0], &tied[1], &tied[2]]),
        "equal names order by a.id"
    );
}

#[sqlx::test]
async fn equal_years_fall_back_to_the_canonical_id(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let mut tied_years = Vec::new();

    for index in 0..3u32 {
        let album_row = repository
            .resolve_or_create_album(
                &album(&format!("Year {index}"))
                    .with_year(Some(2000))
                    .unwrap(),
            )
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("tied-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        tied_years.push(album_row.id().clone());
    }

    let expected = sorted(&[&tied_years[0], &tied_years[1], &tied_years[2]]);

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::MaxYear,
            &AlbumListFilter::new(),
            None
        )
        .await,
        expected,
        "equal years order by a.id"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::MaxYear,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            None,
        )
        .await,
        expected,
        "and by a.id in the other direction too"
    );
}

#[sqlx::test]
async fn value_order_for_song_count_and_duration(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut counts = Vec::new();

    for (title, tracks) in [
        ("Wide", vec![100u64, 100, 100]),
        ("Middle", vec![1_000, 1_000]),
        ("Middle2", vec![500, 1_500]),
        ("Narrow", vec![5_000]),
    ] {
        let album_row = repository
            .resolve_or_create_album(&album(title))
            .await
            .unwrap();

        for (index, duration) in tracks.into_iter().enumerate() {
            let track = repository
                .get_or_create(
                    source("local", &format!("{title}-{index}.flac")),
                    TrackMetadata::from_names(
                        format!("{title} {index}"),
                        vec!["Narrator".to_owned()],
                        Some(duration),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            place(&repository, album_row.id(), track.id()).await;
        }

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM album_tracks WHERE album_id = $1")
                .bind(album_row.id().as_uuid())
                .fetch_one(&pool)
                .await
                .unwrap();
        counts.push((album_row.id().clone(), count));
    }

    let mut by_count_desc = counts.clone();
    by_count_desc.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.as_uuid().cmp(right.0.as_uuid()))
    });
    let mut by_count_asc = counts.clone();
    by_count_asc.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.as_uuid().cmp(right.0.as_uuid()))
    });
    let ids = |pairs: &[(AlbumId, i64)]| pairs.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::SongCount,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        ids(&by_count_desc),
        "song_count defaults to descending, counting distinct member tracks"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::SongCount,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            None,
        )
        .await,
        ids(&by_count_asc),
        "song_count ascending is the reverse membership"
    );

    let summaries = repository
        .list_albums(0, 500, AlbumOrder::Duration, AlbumListFilter::new(), None)
        .await
        .unwrap();
    let mut facts = BTreeMap::new();

    for summary in &summaries {
        facts.insert(
            summary.album().title().to_owned(),
            (summary.duration_ms(), summary.track_count()),
        );
    }

    assert_eq!(facts["Wide"], (300, 3));
    assert_eq!(facts["Middle"], (2_000, 2));
    assert_eq!(facts["Middle2"], (2_000, 2));
    assert_eq!(facts["Narrow"], (5_000, 1));

    let mut by_duration = summaries
        .iter()
        .map(|summary| {
            (
                summary.album().id().clone(),
                i64::try_from(summary.duration_ms()).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    by_duration.sort_by(|left, right| {
        left.1
            .cmp(&right.1)
            .then_with(|| left.0.as_uuid().cmp(right.0.as_uuid()))
    });

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::Duration,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        ids(&by_duration),
        "duration defaults to ascending and sums the member tracks"
    );

    let mut descending = by_duration.clone();
    descending.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.as_uuid().cmp(right.0.as_uuid()))
    });

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::Duration,
            &AlbumListFilter::new().with_sort_direction(Some(SortDirection::Descending)),
            None,
        )
        .await,
        ids(&descending),
        "duration descending is not the song_count order"
    );
    assert_ne!(ids(&by_count_desc), ids(&descending));
}

#[sqlx::test]
async fn play_count_selector_and_sort_read_the_same_aggregate(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("play-count", "Play Count")
        .await
        .unwrap();
    let mut albums = Vec::new();

    for (index, (title, plays)) in [("Played", 2u32), ("Once", 1), ("Silent", 0)]
        .into_iter()
        .enumerate()
    {
        let album_row = repository
            .resolve_or_create_album(&album(title))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("plays-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;

        for _ in 0..plays {
            seed_play(&pool, user, track.id(), chrono::Utc::now()).await;
        }

        albums.push(album_row.id().clone());
    }

    let ordered = repository
        .list_albums(
            0,
            500,
            AlbumOrder::PlayCount,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    let counts = ordered
        .iter()
        .map(|summary| {
            (
                summary.album().title().to_owned(),
                summary.state().play_count(),
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(
        counts,
        vec![
            ("Played".to_owned(), 2),
            ("Once".to_owned(), 1),
            ("Silent".to_owned(), 0)
        ],
        "the play_count sort keeps the unplayed album last"
    );

    let played = AlbumListFilter::new().with_play_count(Some(range(Some(1), None)));

    assert_eq!(
        listed(&repository, AlbumOrder::PlayCount, &played, Some(&user)).await,
        vec![albums[0].clone(), albums[1].clone()],
        "the selector returns exactly the albums the sort orders above zero"
    );
    assert_eq!(total(&repository, &played, Some(&user)).await, 2);

    let twice = AlbumListFilter::new().with_play_count(Some(range(Some(2), None)));

    assert_eq!(
        listed(&repository, AlbumOrder::PlayCount, &twice, Some(&user)).await,
        vec![albums[0].clone()]
    );
    assert_eq!(total(&repository, &twice, Some(&user)).await, 1);

    let unplayed = AlbumListFilter::new().with_play_count(Some(range(Some(0), Some(0))));

    assert_eq!(
        listed(&repository, AlbumOrder::PlayCount, &unplayed, Some(&user)).await,
        vec![albums[2].clone()],
        "a zero bound matches the album with no member plays"
    );
    assert_eq!(total(&repository, &unplayed, Some(&user)).await, 1);
}

#[sqlx::test]
async fn state_sorts_keep_entities_without_the_state(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("state-sort", "State Sort")
        .await
        .unwrap();
    let mut albums = Vec::new();

    for (index, title) in ["Starred", "Rated", "Played", "Plain"]
        .into_iter()
        .enumerate()
    {
        let album_row = repository
            .resolve_or_create_album(&album(title))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("state-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        albums.push((album_row.id().clone(), track.id().clone()));
    }

    repository
        .set_starred(user, StateTarget::Album(&albums[0].0), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Album(&albums[1].0), Some(5))
        .await
        .unwrap();
    seed_play(&pool, user, &albums[2].1, chrono::Utc::now()).await;
    age_album_favorite(&pool, &albums[0].0, user, -1).await;

    let expected = sorted(&[&albums[0].0, &albums[1].0, &albums[2].0, &albums[3].0]);

    for (order, first, last) in [
        (AlbumOrder::StarredAt, &albums[0].0, &albums[3].0),
        (AlbumOrder::Rating, &albums[1].0, &albums[3].0),
        (AlbumOrder::PlayCount, &albums[2].0, &albums[3].0),
        (AlbumOrder::PlayDate, &albums[2].0, &albums[3].0),
    ] {
        let observed = listed(&repository, order, &AlbumListFilter::new(), Some(&user)).await;

        assert_eq!(
            observed.len(),
            4,
            "{order:?} keeps every engaged album instead of shortening the list"
        );
        assert_eq!(
            observed
                .iter()
                .map(|id| *id.as_uuid())
                .collect::<BTreeSet<_>>(),
            expected
                .iter()
                .map(|id| *id.as_uuid())
                .collect::<BTreeSet<_>>(),
            "{order:?} returns the same membership"
        );
        assert_eq!(observed[0], first.clone(), "{order:?} leads with the state");
        assert_eq!(
            observed[3],
            last.clone(),
            "{order:?} leaves the entity without the state last"
        );
        assert_eq!(
            total(&repository, &AlbumListFilter::new(), Some(&user)).await,
            4,
            "the {order:?} count agrees"
        );
    }
}

#[sqlx::test]
async fn state_sorts_without_a_principal_are_not_empty(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("anonymous-sort", "Anonymous Sort")
        .await
        .unwrap();
    let mut albums = Vec::new();

    for (index, title) in ["One", "Two", "Three"].into_iter().enumerate() {
        let album_row = repository
            .resolve_or_create_album(&album(title))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("anonymous-{index}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        albums.push(album_row.id().clone());
    }

    repository
        .set_rating(user, StateTarget::Album(&albums[0]), Some(5))
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Album(&albums[1]), true)
        .await
        .unwrap();

    for order in [
        AlbumOrder::Rating,
        AlbumOrder::StarredAt,
        AlbumOrder::PlayCount,
        AlbumOrder::PlayDate,
    ] {
        let observed = listed(&repository, order, &AlbumListFilter::new(), None).await;

        assert_eq!(
            observed,
            sorted(&[&albums[0], &albums[1], &albums[2]]),
            "{order:?} without a principal is the scoped list ordered by a.id, not an empty page"
        );
        assert_eq!(
            total(&repository, &AlbumListFilter::new(), None).await,
            u32::try_from(observed.len()).unwrap(),
            "and the count agrees for {order:?}"
        );
    }
}

#[sqlx::test]
async fn recently_added_and_date_added_are_different_facts(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let early = repository
        .resolve_or_create_album(&album("Early materialised"))
        .await
        .unwrap();
    let late = repository
        .resolve_or_create_album(&album("Late materialised"))
        .await
        .unwrap();

    for (album_row, external) in [(&early, "early.flac"), (&late, "late.flac")] {
        let track = repository
            .get_or_create(source("local", external), metadata_by("member", "Narrator"))
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        repository
            .set_album_source(album_row.id(), &source("local", external))
            .await
            .unwrap();
    }

    age_album(&pool, early.id(), -1).await;
    age_album(&pool, late.id(), -30).await;
    age_album_source(&pool, early.id(), -30).await;
    age_album_source(&pool, late.id(), -1).await;

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::RecentlyAdded,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        vec![early.id().clone(), late.id().clone()],
        "recently_added is the album row's own creation order"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::DateAdded,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        vec![late.id().clone(), early.id().clone()],
        "date_added is the earliest owned album source"
    );

    let recent = AlbumListFilter::new().with_date_added(Some(range(
        Some(chrono::Utc::now() - chrono::Duration::days(7)),
        None,
    )));
    assert_eq!(
        listed(&repository, AlbumOrder::DateAdded, &recent, None).await,
        vec![late.id().clone()],
        "the date_added selector reads the same fact as the sort"
    );
    assert_eq!(total(&repository, &recent, None).await, 1);
}

#[sqlx::test]
async fn album_artist_and_artist_sorts_differ(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let both = repository
        .resolve_or_create_album(&album("Both").with_album_artist(credit("Zeta", None)))
        .await
        .unwrap();
    let album_side = repository
        .resolve_or_create_album(&album("AlbumSide").with_album_artist(credit("Alpha", None)))
        .await
        .unwrap();

    let both_track = repository
        .get_or_create(source("local", "both.flac"), metadata_by("member", "Alpha"))
        .await
        .unwrap();
    let album_side_track = repository
        .get_or_create(
            source("local", "album-side.flac"),
            metadata_by("member", "Zeta"),
        )
        .await
        .unwrap();
    place(&repository, both.id(), both_track.id()).await;
    place(&repository, album_side.id(), album_side_track.id()).await;

    assert_eq!(
        listed(
            &repository,
            AlbumOrder::AlbumArtist,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        vec![album_side.id().clone(), both.id().clone()],
        "album_artist orders by the album-artist name"
    );
    assert_eq!(
        listed(
            &repository,
            AlbumOrder::Artist,
            &AlbumListFilter::new(),
            None
        )
        .await,
        vec![both.id().clone(), album_side.id().clone()],
        "artist orders by the minimum credited-artist name over the member tracks"
    );
}

#[sqlx::test]
async fn compilation_is_tri_state(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut albums = Vec::new();

    for (title, compilation) in [
        ("Compilation", Some(true)),
        ("Solo", Some(false)),
        ("Unknown", None),
    ] {
        let metadata = match compilation {
            Some(value) => album(title).with_compilation(Some(value)),
            None => album(title),
        };
        let album_row = repository.resolve_or_create_album(&metadata).await.unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("{title}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        albums.push(album_row.id().clone());
    }

    let true_filter = AlbumListFilter::new().with_compilation(Some(true));
    let false_filter = AlbumListFilter::new().with_compilation(Some(false));
    let absent = AlbumListFilter::new();

    assert_eq!(
        listed(&repository, AlbumOrder::Name, &true_filter, None).await,
        vec![albums[0].clone()]
    );
    assert_eq!(total(&repository, &true_filter, None).await, 1);
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &false_filter, None).await,
        vec![albums[1].clone()],
        "false matches only a stored false, never an unobserved value"
    );
    assert_eq!(total(&repository, &false_filter, None).await, 1);
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &absent, None).await,
        sorted(&[&albums[0], &albums[1], &albums[2]]),
        "an absent selector returns the unobserved album too"
    );
    assert_eq!(total(&repository, &absent, None).await, 3);
}

#[sqlx::test]
async fn missing_is_scoped_to_present_local_sources(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("missing-user", "Missing User")
        .await
        .unwrap();
    let missing = repository
        .resolve_or_create_album(&album("Missing local"))
        .await
        .unwrap();
    let provider_only = repository
        .resolve_or_create_album(&album("Provider only"))
        .await
        .unwrap();
    let unwalked = repository
        .resolve_or_create_album(&album("Unwalked root"))
        .await
        .unwrap();

    let missing_track = repository
        .get_or_create(
            source("local", "missing.flac"),
            metadata_by("member", "Narrator"),
        )
        .await
        .unwrap();
    let provider_track = repository
        .get_or_create(
            source("youtube_music", "vid-missing"),
            metadata_by("member", "Narrator"),
        )
        .await
        .unwrap();
    let unwalked_track = repository
        .get_or_create(
            source_in("local", "/unwalked", "unwalked.flac"),
            metadata_by("member", "Narrator"),
        )
        .await
        .unwrap();

    place(&repository, missing.id(), missing_track.id()).await;
    place(&repository, provider_only.id(), provider_track.id()).await;
    place(&repository, unwalked.id(), unwalked_track.id()).await;
    set_missing(&pool, &source("local", "missing.flac"), true).await;
    repository
        .set_starred(user, StateTarget::Album(provider_only.id()), true)
        .await
        .unwrap();

    let missing_filter = AlbumListFilter::new().with_missing(Some(true));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &missing_filter, Some(&user)).await,
        vec![missing.id().clone()],
        "only an album with a present local member source marked missing"
    );
    assert_eq!(total(&repository, &missing_filter, Some(&user)).await, 1);

    let present = AlbumListFilter::new().with_missing(Some(false));
    assert_eq!(
        listed(&repository, AlbumOrder::Name, &present, Some(&user)).await,
        vec![provider_only.id().clone(), unwalked.id().clone()],
        "a provider-only album and an unwalked root are both not missing"
    );
    assert_eq!(total(&repository, &present, Some(&user)).await, 2);
}

#[sqlx::test]
async fn the_scope_carrier_is_the_filter_not_the_order(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("scope-carrier", "Scope Carrier")
        .await
        .unwrap();
    let owned = repository
        .resolve_or_create_album(&album("Owned"))
        .await
        .unwrap();
    let remote = repository
        .resolve_or_create_album(&album("Remote"))
        .await
        .unwrap();

    let owned_track = repository
        .get_or_create(
            source("local", "carrier.flac"),
            metadata_by("member", "Narrator"),
        )
        .await
        .unwrap();
    let remote_track = repository
        .get_or_create(
            source("youtube_music", "vid-carrier"),
            metadata_by("member", "Narrator"),
        )
        .await
        .unwrap();
    place(&repository, owned.id(), owned_track.id()).await;
    place(&repository, remote.id(), remote_track.id()).await;
    repository
        .set_starred(user, StateTarget::Album(remote.id()), true)
        .await
        .unwrap();

    for order in [
        AlbumOrder::RecentlyAdded,
        AlbumOrder::DateAdded,
        AlbumOrder::Name,
    ] {
        assert_eq!(
            listed(&repository, order, &AlbumListFilter::new(), Some(&user))
                .await
                .iter()
                .map(|id| *id.as_uuid())
                .collect::<BTreeSet<_>>(),
            id_set(&[owned.id(), remote.id()]),
            "{order:?} under the default engaged scope includes the engaged provider album"
        );
        assert_eq!(
            listed(
                &repository,
                order,
                &AlbumListFilter::new().with_scope(AlbumListScope::Owned),
                Some(&user),
            )
            .await,
            vec![owned.id().clone()],
            "{order:?} under owned excludes it"
        );
        assert_eq!(
            total(
                &repository,
                &AlbumListFilter::new().with_scope(AlbumListScope::Owned),
                Some(&user),
            )
            .await,
            1,
            "the {order:?} owned count agrees"
        );
        assert_eq!(
            total(&repository, &AlbumListFilter::new(), Some(&user)).await,
            2,
            "and the engaged count agrees"
        );
    }
}

#[sqlx::test]
async fn random_is_repeatable_per_seed(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    for index in 0..40u32 {
        let album_row = repository
            .resolve_or_create_album(&album(&format!("random {index:03}")))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("random-{index:03}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
    }

    let first = AlbumListFilter::new().with_random_seed(Some(RandomSeed::new(4_242)));
    let second = AlbumListFilter::new().with_random_seed(Some(RandomSeed::new(9_999)));
    let whole = listed(&repository, AlbumOrder::Random, &first, None).await;
    let other_seed = listed(&repository, AlbumOrder::Random, &second, None).await;

    assert_eq!(
        listed_at(&repository, 0, 10, AlbumOrder::Random, &first, None).await,
        listed_at(&repository, 0, 10, AlbumOrder::Random, &first, None).await,
        "the same seed repeats the same page"
    );
    assert_eq!(
        listed_at(&repository, 10, 10, AlbumOrder::Random, &first, None).await,
        listed_at(&repository, 10, 10, AlbumOrder::Random, &first, None).await,
        "and repeats a later page"
    );
    assert_eq!(
        whole
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<BTreeSet<_>>()
            .len(),
        40,
        "the random order is a permutation of the membership"
    );
    assert_eq!(total(&repository, &first, None).await, 40);
    assert_eq!(total(&repository, &second, None).await, 40);
    assert_ne!(whole, other_seed, "a different seed is a different order");
    assert_eq!(
        whole.iter().take(10).cloned().collect::<Vec<_>>(),
        listed_at(&repository, 0, 10, AlbumOrder::Random, &first, None).await,
        "the first page is the head of the same permutation"
    );
    assert_eq!(
        whole.iter().skip(10).take(10).cloned().collect::<Vec<_>>(),
        listed_at(&repository, 10, 10, AlbumOrder::Random, &first, None).await,
        "the second page continues the same permutation"
    );

    let unknown = AlbumListFilter::new().with_random_seed(Some(RandomSeed::new(1)));
    assert!(
        repository
            .list_albums(0, 10, AlbumOrder::Name, unknown, None)
            .await
            .is_err(),
        "a seed with a non-random sort is rejected, not ignored"
    );
}

#[sqlx::test]
async fn deep_page_agrees_with_the_count(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut fixture = Vec::new();

    for index in 0..1_001u32 {
        let title = format!("item{:02}", index % 10);
        let metadata =
            album(&title).with_album_artist(credit(&format!("band{:03}", index / 10), None));
        let album_row = repository.resolve_or_create_album(&metadata).await.unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("deep-{index:04}.flac")),
                metadata_by("member", "Narrator"),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        fixture.push((title, album_row.id().clone()));
    }

    let mut expected = fixture.clone();
    expected.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_uuid().cmp(right.1.as_uuid()))
    });
    let expected = expected.into_iter().map(|(_, id)| id).collect::<Vec<_>>();

    assert_eq!(expected.len(), 1_001);
    assert_eq!(
        listed_at(
            &repository,
            0,
            1_001,
            AlbumOrder::Name,
            &AlbumListFilter::new(),
            None,
        )
        .await,
        expected,
        "the whole 1001-album list is the title order with the id tie-break"
    );

    let mut totals = BTreeSet::new();

    for offset in [0u32, 500, 1_000] {
        let observed = listed_at(
            &repository,
            offset,
            500,
            AlbumOrder::Name,
            &AlbumListFilter::new(),
            None,
        )
        .await;
        let expected_page = expected
            .iter()
            .skip(offset as usize)
            .take(500)
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(observed, expected_page, "the deep page at {offset}");
        totals.insert(total(&repository, &AlbumListFilter::new(), None).await);
    }

    assert_eq!(
        totals,
        BTreeSet::from([1_001]),
        "the count agrees at every offset"
    );
}

#[test]
fn unknown_and_unsupported_inputs_are_explicit() {
    assert_eq!(AlbumOrder::ALL.len(), 13);
    assert_eq!(AlbumOrder::parse("name"), Some(AlbumOrder::Name));
    assert_eq!(AlbumOrder::parse("song_count"), Some(AlbumOrder::SongCount));
    assert_eq!(AlbumOrder::parse("explicitStatus"), None);
    assert_eq!(AlbumOrder::parse("songCount"), None);
    assert_eq!(AlbumOrder::parse("year"), None);
    assert_eq!(AlbumOrder::parse("title"), None);
    assert_eq!(AlbumOrder::parse("Name"), None);
    assert_eq!(AlbumOrder::parse(""), None);
    assert_eq!(AlbumOrder::parse("frequent"), None);
    assert_eq!(AlbumListScope::parse("owned"), Some(AlbumListScope::Owned));
    assert_eq!(AlbumListScope::parse("all"), None);
    assert_eq!(SortDirection::parse("sideways"), None);
    assert!(matches!(
        Range::new(Some(5), Some(1)),
        Err(ValidationError::Invalid { field: "range" })
    ));
    assert!(matches!(
        IdSet::new((0..=100u8).collect::<Vec<_>>()),
        Err(ValidationError::TooLong { field: "ids" })
    ));
    assert!(matches!(
        SelectorText::new(&"a".repeat(501)),
        Err(ValidationError::TooLong { field: "selector" })
    ));
    assert_eq!(SelectorText::new("   ").unwrap(), None);
    assert_eq!(
        AlbumListFilter::new()
            .with_random_seed(Some(RandomSeed::new(1)))
            .resolved_seed(AlbumOrder::Name),
        Err(ValidationError::Invalid {
            field: "random_seed"
        })
    );
    assert_eq!(
        AlbumListFilter::new()
            .resolved_seed(AlbumOrder::Random)
            .unwrap(),
        None
    );

    let absent = AlbumListFilter::new();

    assert_eq!(absent.scope(), AlbumListScope::Engaged);
    assert!(absent.name().is_none());
    assert!(absent.artist_id().is_none());
    assert!(absent.album_artist_id().is_none());
    assert!(absent.artists_id().is_none());
    assert!(absent.role().is_none());
    assert!(absent.genre().is_none());
    assert!(absent.year().is_none());
    assert!(absent.compilation().is_none());
    assert!(absent.starred().is_none());
    assert!(absent.has_rating().is_none());
    assert!(absent.rating().is_none());
    assert!(absent.recently_played().is_none());
    assert!(absent.date_added().is_none());
    assert!(absent.missing().is_none());
    assert!(absent.library_id().is_none());
    assert!(absent.sort_direction().is_none());
    assert!(absent.random_seed().is_none());

    let one_bound = Range::new(Some(1999), None).unwrap();

    assert_eq!(one_bound.from(), Some(1999));
    assert_eq!(one_bound.to(), None);
    assert_eq!(
        AlbumListFilter::new().with_year(Some(one_bound)).year(),
        Some(one_bound),
        "a one-bound range is a valid year selector"
    );

    let single = Range::new(Some(1999), Some(1999)).unwrap();

    assert_eq!(single.from(), Some(1999));
    assert_eq!(single.to(), Some(1999));
    assert_eq!(
        AlbumListFilter::new().with_year(Some(single)).year(),
        Some(single),
        "a single year is a range whose bounds are equal"
    );
    assert_eq!(
        AlbumListFilter::new().with_year(None).year(),
        None,
        "an absent year is no filter at all"
    );
    assert_eq!(
        AlbumOrder::Name.default_direction(),
        SortDirection::Ascending
    );
    assert_eq!(
        AlbumOrder::MaxYear.default_direction(),
        SortDirection::Descending
    );
    assert_eq!(
        AlbumOrder::SongCount.default_direction(),
        SortDirection::Descending
    );
    assert_eq!(
        AlbumListFilter::new()
            .with_sort_direction(Some(SortDirection::Descending))
            .resolved_direction(AlbumOrder::Name),
        SortDirection::Descending
    );
}
