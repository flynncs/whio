use std::collections::BTreeSet;

use sqlx::PgPool;
use uuid::Uuid;

use super::PostgresTrackRepository;
use crate::features::identity::provision;
use crate::features::music_library::{
    domain::{
        AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ArtistId, CreditRole, IdSet, ProviderId,
        RandomSeed, Range, RoleSelector, RoleSet, SelectorText, SortDirection, SourceIdentity,
        SourceRepresentationFacts, SourceScope, StateTarget, TrackId, TrackListFilter,
        TrackListScope, TrackMetadata, TrackOrder, ValidationError,
    },
    library::LibraryId,
    library_state::test_support::seed_play,
    storage::contracts::{
        CatalogueWrite, LibraryRead, SourceFileFacts, StateStore, TrackRepositoryError,
    },
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

fn metadata(title: &str) -> TrackMetadata {
    metadata_by(title, "Narrator")
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

fn album_id_set(ids: &[&AlbumId]) -> IdSet<AlbumId> {
    IdSet::new(ids.iter().map(|id| (*id).clone())).unwrap()
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

fn page_ids(page: &crate::features::music_library::domain::TrackPage) -> Vec<TrackId> {
    page.items()
        .iter()
        .map(|item| item.track().id().clone())
        .collect()
}

fn sorted(ids: &[&TrackId]) -> Vec<TrackId> {
    let mut ids = ids.iter().map(|id| (*id).clone()).collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    ids
}

async fn page_at(
    repository: &PostgresTrackRepository,
    offset: u32,
    limit: u32,
    order: TrackOrder,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> Vec<TrackId> {
    repository
        .page_tracks(offset, limit, order, filter, user.copied())
        .await
        .unwrap()
        .into_parts()
        .0
        .iter()
        .map(|item| item.track().id().clone())
        .collect()
}

async fn total(
    repository: &PostgresTrackRepository,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> u32 {
    total_of(repository, TrackOrder::Id, filter, user).await
}

async fn total_of(
    repository: &PostgresTrackRepository,
    order: TrackOrder,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> u32 {
    repository
        .page_tracks(0, 500, order, filter, user.copied())
        .await
        .unwrap()
        .total()
}

async fn list(
    repository: &PostgresTrackRepository,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> Vec<TrackId> {
    page_at(repository, 0, 500, TrackOrder::Id, filter, user).await
}

async fn album_page_at(
    repository: &PostgresTrackRepository,
    album: &AlbumId,
    offset: u32,
    limit: u32,
    order: TrackOrder,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> Vec<TrackId> {
    repository
        .page_album_tracks(album, offset, limit, order, filter, user.copied())
        .await
        .unwrap()
        .into_parts()
        .0
        .iter()
        .map(|item| item.track().id().clone())
        .collect()
}

async fn album_total(
    repository: &PostgresTrackRepository,
    album: &AlbumId,
    filter: &TrackListFilter,
    user: Option<&Uuid>,
) -> u32 {
    repository
        .page_album_tracks(album, 0, 500, TrackOrder::Id, filter, user.copied())
        .await
        .unwrap()
        .total()
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

fn local_facts(channels: Option<u8>, bitrate_kbps: Option<u32>) -> SourceFileFacts {
    SourceFileFacts {
        path: Some("Library/song.flac".to_owned()),
        size_bytes: 4_000,
        modified_at: None,
        missing_at: None,
        content_type: Some("audio/flac".to_owned()),
        suffix: Some("flac".to_owned()),
        container: Some("flac".to_owned()),
        codec: Some("flac".to_owned()),
        bitrate_kbps,
        sample_rate_hz: Some(44_100),
        bit_depth: Some(16),
        channels,
        facts_version: 1,
        acoustid_fingerprint: None,
    }
}

fn encoded(source: &SourceIdentity) -> String {
    match source.source_scope() {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(root) => format!("named:{root}"),
    }
}

async fn add_local_source(pool: &PgPool, track: &TrackId, external_id: &str) {
    sqlx::query(
        "INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id, created_at) \
         VALUES ($1, $2, 'local', $3, $4, CURRENT_TIMESTAMP)",
    )
    .bind(Uuid::now_v7())
    .bind(track.as_uuid())
    .bind(encoded(&source("local", external_id)))
    .bind(external_id)
    .execute(pool)
    .await
    .unwrap();
}

async fn attach_album_track(
    pool: &PgPool,
    album: &AlbumId,
    track: &TrackId,
    disc_number: i16,
    track_number: i16,
    position: i32,
) {
    sqlx::query(
        "INSERT INTO album_tracks (album_id, track_id, disc_number, track_number, position) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(album.as_uuid())
    .bind(track.as_uuid())
    .bind(disc_number)
    .bind(track_number)
    .bind(position)
    .execute(pool)
    .await
    .unwrap();
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

async fn age_track(pool: &PgPool, track: &TrackId, days: i64) {
    sqlx::query("UPDATE tracks SET created_at = $1 WHERE id = $2")
        .bind(chrono::Utc::now() + chrono::Duration::days(days))
        .bind(track.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

async fn age_source(pool: &PgPool, source: &SourceIdentity, days: i64) {
    sqlx::query(
        "UPDATE track_sources SET created_at = $4 \
         WHERE provider_id = $1 AND source_scope = $2 AND external_id = $3",
    )
    .bind(source.provider_id().as_str())
    .bind(encoded(source))
    .bind(source.external_id())
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

async fn table_scans(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COALESCE(seq_scan, 0) + COALESCE(idx_scan, 0) FROM pg_stat_user_tables \
         WHERE relname = $1",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn settled_table_scans(pool: &PgPool, table: &str) -> i64 {
    let mut previous = -1;

    for _ in 0..4 {
        sqlx::query("SELECT pg_stat_force_next_flush()")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("SELECT 1").execute(pool).await.unwrap();
        let current = table_scans(pool, table).await;

        if current == previous {
            return current;
        }
        previous = current;
    }

    previous
}

#[sqlx::test]
async fn or_within_and_across_selectors(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_album = repository
        .resolve_or_create_album(&album("Shared").with_album_artist(credit("Narrator", None)))
        .await
        .unwrap();
    let second_album = repository
        .resolve_or_create_album(&album("Other").with_album_artist(credit("Narrator", None)))
        .await
        .unwrap();
    let doubled = repository
        .get_or_create(
            source("local", "doubled.flac"),
            metadata_by("doubled", "Doubled")
                .with_genres(vec!["Rock".to_owned()])
                .with_credits(vec![
                    credit("Doubled", Some(CreditRole::Composer)),
                    credit("Doubled", Some(CreditRole::Performer)),
                ]),
        )
        .await
        .unwrap();
    let pop = repository
        .get_or_create(
            source("local", "pop.flac"),
            metadata("pop").with_genres(vec!["Pop".to_owned()]),
        )
        .await
        .unwrap();
    let third = repository
        .get_or_create(
            source("local", "third.flac"),
            metadata("third").with_genres(vec!["Rock".to_owned()]),
        )
        .await
        .unwrap();

    let doubled_id = doubled.id().clone();
    let pop_id = pop.id().clone();
    let third_id = third.id().clone();

    repository
        .set_album_track(first_album.id(), &doubled_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(first_album.id(), &pop_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(second_album.id(), &third_id, &AlbumTrack::unplaced())
        .await
        .unwrap();

    let doubled_artist = credit_artist(&pool, &doubled_id, "Doubled").await;
    let both_albums = TrackListFilter::new()
        .with_album_id(Some(album_id_set(&[first_album.id(), second_album.id()])));

    assert_eq!(
        list(&repository, &both_albums, None).await,
        sorted(&[&doubled_id, &pop_id, &third_id]),
        "repeated album_id values are an OR"
    );
    assert_eq!(total(&repository, &both_albums, None).await, 3);

    let one_album = TrackListFilter::new().with_album_id(Some(album_id_set(&[first_album.id()])));
    assert_eq!(
        list(&repository, &one_album, None).await,
        sorted(&[&doubled_id, &pop_id])
    );

    let album_and_genre = one_album.clone().with_genre(Some(text_set(&["rock"])));
    assert_eq!(
        list(&repository, &album_and_genre, None).await,
        sorted(&[&doubled_id]),
        "two selectors are ANDed"
    );
    assert_eq!(total(&repository, &album_and_genre, None).await, 1);

    let credited = TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&doubled_artist])));
    assert_eq!(
        list(&repository, &credited, None).await,
        sorted(&[&doubled_id]),
        "a track crediting the artist three times appears once"
    );
    assert_eq!(
        total(&repository, &credited, None).await,
        1,
        "and the total counts it once"
    );
}

#[sqlx::test]
async fn total_agrees_with_the_list_for_every_selector(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("selector-user", "Selector User")
        .await
        .unwrap();
    let kept_album = repository
        .resolve_or_create_album(
            &album("Kept")
                .with_album_artist(credit("Album Artist", None))
                .with_year(Some(1999))
                .unwrap()
                .with_compilation(Some(false)),
        )
        .await
        .unwrap();
    let other_album = repository
        .resolve_or_create_album(&album("Other").with_year(Some(2020)).unwrap())
        .await
        .unwrap();
    let alpha = repository
        .get_or_create(
            source("local", "alpha.flac"),
            metadata_by("alpha", "Composer One")
                .with_genres(vec!["Rock".to_owned(), "Pop".to_owned()])
                .with_credits(vec![credit("Composer One", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    let beta = repository
        .get_or_create(
            source_in("local", OTHER_ROOT, "beta.flac"),
            metadata_by("beta", "Narrator")
                .with_genres(vec!["Jazz".to_owned()])
                .with_credits(vec![credit("Narrator", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    let delta = repository
        .get_or_create(source("local", "delta.flac"), metadata("delta"))
        .await
        .unwrap();
    let compilation_album = repository
        .resolve_or_create_album(&album("Various").with_compilation(Some(true)))
        .await
        .unwrap();
    let epsilon = repository
        .get_or_create(source("local", "epsilon.flac"), metadata("epsilon"))
        .await
        .unwrap();

    let alpha_id = alpha.id().clone();
    let beta_id = beta.id().clone();
    let delta_id = delta.id().clone();
    let epsilon_id = epsilon.id().clone();

    repository
        .set_album_track(kept_album.id(), &alpha_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(other_album.id(), &beta_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(compilation_album.id(), &epsilon_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source("local", "alpha.flac"),
            &local_facts(Some(2), Some(320)),
        )
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source_in("local", OTHER_ROOT, "beta.flac"),
            &local_facts(Some(6), Some(1411)),
        )
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Track(&alpha_id), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(&alpha_id), Some(4))
        .await
        .unwrap();
    seed_play(&pool, user, &beta_id, chrono::Utc::now()).await;
    set_missing(&pool, &source_in("local", OTHER_ROOT, "beta.flac"), true).await;

    let alpha_artist = credit_artist(&pool, &alpha_id, "Composer One").await;
    let album_artist = album_artist_of(&pool, kept_album.id()).await;
    let music = register_library(&pool, "main", LOCAL_ROOT).await;
    let elsewhere = register_library(&pool, "elsewhere", OTHER_ROOT).await;

    let fixtures: Vec<(&str, TrackListFilter, Vec<&TrackId>)> = vec![
        (
            "title",
            TrackListFilter::new().with_title(selector("alpha")),
            vec![&alpha_id],
        ),
        (
            "album_id",
            TrackListFilter::new().with_album_id(Some(album_id_set(&[kept_album.id()]))),
            vec![&alpha_id],
        ),
        (
            "album_artist_id",
            TrackListFilter::new().with_album_artist_id(Some(artist_id_set(&[&album_artist]))),
            vec![&alpha_id],
        ),
        (
            "artists_id",
            TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&alpha_artist]))),
            vec![&alpha_id],
        ),
        (
            "role",
            TrackListFilter::new().with_role(Some(role_set(&[RoleSelector::Composer]))),
            vec![&alpha_id, &beta_id],
        ),
        (
            "genre",
            TrackListFilter::new().with_genre(Some(text_set(&["rock"]))),
            vec![&alpha_id],
        ),
        (
            "year",
            TrackListFilter::new().with_year(Some(range(Some(1990), Some(2010)))),
            vec![&alpha_id],
        ),
        (
            "compilation",
            TrackListFilter::new().with_compilation(Some(false)),
            vec![&alpha_id],
        ),
        (
            "compilation true",
            TrackListFilter::new().with_compilation(Some(true)),
            vec![&epsilon_id],
        ),
        (
            "starred",
            TrackListFilter::new().with_starred(Some(true)),
            vec![&alpha_id],
        ),
        (
            "has_rating",
            TrackListFilter::new().with_has_rating(Some(true)),
            vec![&alpha_id],
        ),
        (
            "rating",
            TrackListFilter::new().with_rating(Some(range(Some(4), Some(5)))),
            vec![&alpha_id],
        ),
        (
            "play_count",
            TrackListFilter::new().with_play_count(Some(range(Some(1), None))),
            vec![&beta_id],
        ),
        (
            "recently_played",
            TrackListFilter::new().with_recently_played(Some(true)),
            vec![&beta_id],
        ),
        (
            "missing",
            TrackListFilter::new().with_missing(Some(true)),
            vec![&beta_id],
        ),
        (
            "path",
            TrackListFilter::new().with_path(selector("song.flac")),
            vec![&alpha_id, &beta_id],
        ),
        (
            "duration",
            TrackListFilter::new().with_duration(Some(range(Some(500), Some(2_000)))),
            vec![&alpha_id, &beta_id, &delta_id, &epsilon_id],
        ),
        (
            "bitrate",
            TrackListFilter::new().with_bitrate(Some(range(Some(300), Some(400)))),
            vec![&alpha_id],
        ),
        (
            "bitdepth",
            TrackListFilter::new().with_bitdepth(Some(range(Some(16), Some(16)))),
            vec![&alpha_id, &beta_id],
        ),
        (
            "samplerate",
            TrackListFilter::new().with_samplerate(Some(range(Some(44_100), None))),
            vec![&alpha_id, &beta_id],
        ),
        (
            "size",
            TrackListFilter::new().with_size(Some(range(Some(1_000), None))),
            vec![&alpha_id, &beta_id],
        ),
        (
            "channels",
            TrackListFilter::new().with_channels(Some(range(Some(2), Some(2)))),
            vec![&alpha_id],
        ),
        (
            "codec",
            TrackListFilter::new().with_codec(selector("flac")),
            vec![&alpha_id, &beta_id],
        ),
        (
            "filetype",
            TrackListFilter::new().with_filetype(selector("flac")),
            vec![&alpha_id, &beta_id],
        ),
        (
            "library_id",
            TrackListFilter::new().with_library_id(Some(IdSet::new([music.clone()]).unwrap())),
            vec![&alpha_id, &delta_id, &epsilon_id],
        ),
        (
            "library_id elsewhere",
            TrackListFilter::new().with_library_id(Some(IdSet::new([elsewhere]).unwrap())),
            vec![&beta_id],
        ),
    ];

    for (name, filter, expected) in fixtures {
        let expected = sorted(&expected);
        let observed = list(&repository, &filter, Some(&user)).await;
        let observed_total = total(&repository, &filter, Some(&user)).await;

        assert_eq!(observed, expected, "the {name} selector's list");
        assert_eq!(
            observed_total,
            u32::try_from(expected.len()).unwrap(),
            "the {name} selector's total"
        );

        let past_end = repository
            .page_tracks(
                u32::try_from(expected.len()).unwrap() + 5,
                500,
                TrackOrder::Id,
                &filter,
                Some(user),
            )
            .await
            .unwrap();

        assert!(past_end.items().is_empty(), "the {name} page past the end");
        assert_eq!(
            past_end.total(),
            observed_total,
            "the {name} total past the end is unchanged"
        );
    }

    let all = TrackListFilter::new()
        .with_album_id(Some(album_id_set(&[kept_album.id()])))
        .with_genre(Some(text_set(&["rock"])))
        .with_compilation(Some(false))
        .with_starred(Some(true))
        .with_has_rating(Some(true));

    assert_eq!(
        list(&repository, &all, Some(&user)).await,
        sorted(&[&alpha_id]),
        "every selector at once"
    );
    assert_eq!(total(&repository, &all, Some(&user)).await, 1);

    let missing = LibraryId::from_uuid(Uuid::now_v7());
    let unknown =
        TrackListFilter::new().with_library_id(Some(IdSet::new([missing.clone()]).unwrap()));
    assert!(
        matches!(
            repository
                .page_tracks(0, 500, TrackOrder::Id, &unknown, Some(user))
                .await,
            Err(TrackRepositoryError::UnknownLibraryId { id }) if id == missing.to_string()
        ),
        "an unregistered library id is rejected, not ignored"
    );
}

#[sqlx::test]
async fn role_scoped_membership_matches_the_list(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&album("Roles").with_album_artist(credit("X", None)))
        .await
        .unwrap();
    let primary_x = repository
        .get_or_create(
            source("local", "primary-x.flac"),
            metadata_by("primary x", "X")
                .with_credits(vec![credit("Y", Some(CreditRole::Performer))]),
        )
        .await
        .unwrap();
    let composer_x = repository
        .get_or_create(
            source("local", "composer-x.flac"),
            metadata_by("composer x", "Y")
                .with_credits(vec![credit("X", Some(CreditRole::Composer))]),
        )
        .await
        .unwrap();
    let featured = repository
        .get_or_create(
            source("local", "featured.flac"),
            metadata_by("featured", "Y"),
        )
        .await
        .unwrap();

    let primary_id = primary_x.id().clone();
    let composer_id = composer_x.id().clone();
    let featured_id = featured.id().clone();

    repository
        .set_album_track(album.id(), &featured_id, &AlbumTrack::unplaced())
        .await
        .unwrap();

    let x = album_artist_of(&pool, album.id()).await;
    let role = |role: RoleSelector, artists: bool| {
        let filter = TrackListFilter::new().with_role(Some(role_set(&[role])));

        if artists {
            filter.with_artists_id(Some(artist_id_set(&[&x])))
        } else {
            filter
        }
    };

    let composer = role(RoleSelector::Composer, true);
    assert_eq!(
        list(&repository, &composer, None).await,
        sorted(&[&composer_id]),
        "artists_id with role=composer is the composer credit"
    );
    assert_eq!(total(&repository, &composer, None).await, 1);

    assert_eq!(
        list(&repository, &role(RoleSelector::Artist, true), None).await,
        sorted(&[&primary_id]),
        "artists_id with role=artist is the primary credit"
    );
    assert_eq!(
        list(&repository, &role(RoleSelector::AlbumArtist, true), None).await,
        sorted(&[&featured_id]),
        "artists_id with role=albumartist is album-artist membership"
    );
    assert_eq!(
        list(&repository, &role(RoleSelector::Composer, false), None).await,
        sorted(&[&composer_id]),
        "role=composer alone is any composer credit"
    );
    assert_eq!(
        list(&repository, &role(RoleSelector::Artist, false), None).await,
        sorted(&[&primary_id, &composer_id, &featured_id]),
        "role=artist alone is any primary credit"
    );
    assert_eq!(
        list(&repository, &role(RoleSelector::AlbumArtist, false), None).await,
        sorted(&[&featured_id]),
        "role=albumartist alone is album-artist presence"
    );
}

#[sqlx::test]
async fn track_selectors_are_not_aliases(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_album = repository
        .resolve_or_create_album(&album("First").with_album_artist(credit("C2", None)))
        .await
        .unwrap();
    let second_album = repository
        .resolve_or_create_album(&album("Second").with_album_artist(credit("C4", None)))
        .await
        .unwrap();
    let credited = repository
        .get_or_create(
            source("local", "credited.flac"),
            metadata_by("credited", "C1")
                .with_credits(vec![credit("C3", Some(CreditRole::Producer))]),
        )
        .await
        .unwrap();
    let unrelated = repository
        .get_or_create(
            source("local", "unrelated.flac"),
            metadata_by("unrelated", "C5"),
        )
        .await
        .unwrap();
    let unresolved = repository
        .get_or_create(
            source("local", "unresolved.flac"),
            metadata_by("unresolved", "C6"),
        )
        .await
        .unwrap();

    let credited_id = credited.id().clone();
    let unrelated_id = unrelated.id().clone();

    repository
        .set_album_track(first_album.id(), &credited_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(second_album.id(), &unrelated_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    sqlx::query("UPDATE track_artist_credits SET artist_id = NULL WHERE track_id = $1")
        .bind(unresolved.id().as_uuid())
        .execute(&pool)
        .await
        .unwrap();

    let c1 = credit_artist(&pool, &credited_id, "C1").await;
    let c2 = album_artist_of(&pool, first_album.id()).await;
    let c3 = credit_artist(&pool, &credited_id, "C3").await;
    let c5 = credit_artist(&pool, &unrelated_id, "C5").await;

    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&c1]))),
            None
        )
        .await,
        sorted(&[&credited_id]),
        "artists_id selects a credited track artist"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_album_artist_id(Some(artist_id_set(&[&c2]))),
            None
        )
        .await,
        sorted(&[&credited_id]),
        "album_artist_id selects album membership"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&c2]))),
            None
        )
        .await,
        0,
        "the album artist is not a track credit"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_album_artist_id(Some(artist_id_set(&[&c1]))),
            None
        )
        .await,
        0,
        "the track artist is not an album artist"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&c3]))),
            None
        )
        .await,
        sorted(&[&credited_id]),
        "a role credit is still a track credit"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_artists_id(Some(artist_id_set(&[&c1, &c2, &c3, &c5]))),
            None
        )
        .await,
        2,
        "an unresolved credit (artist_id IS NULL) matches no id"
    );
}

#[sqlx::test]
async fn absent_values_sort_last_in_both_directions(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("rating-user", "Rating User")
        .await
        .unwrap();
    let mut rated = Vec::new();

    for (index, rating) in [None, Some(1u8), Some(2), Some(2), Some(3)]
        .into_iter()
        .enumerate()
    {
        let track = repository
            .get_or_create(
                source("local", &format!("rated-{index}.flac")),
                metadata(&format!("rated {index}")),
            )
            .await
            .unwrap();
        let id = track.id().clone();

        if let Some(rating) = rating {
            repository
                .set_rating(user, StateTarget::Track(&id), Some(rating))
                .await
                .unwrap();
        }

        rated.push(id);
    }

    let unrated = rated[0].clone();
    let one = rated[1].clone();
    let three = rated[4].clone();
    let mut equal = [rated[2].clone(), rated[3].clone()];
    equal.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    let ascending = page_at(
        &repository,
        0,
        500,
        TrackOrder::Rating,
        &TrackListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
        Some(&user),
    )
    .await;
    assert_eq!(
        ascending,
        vec![
            one.clone(),
            equal[0].clone(),
            equal[1].clone(),
            three.clone(),
            unrated.clone()
        ],
        "equal ratings fall back to the t.id tie-break and the unrated track sorts last"
    );

    let descending = page_at(
        &repository,
        0,
        500,
        TrackOrder::Rating,
        &TrackListFilter::new(),
        Some(&user),
    )
    .await;
    assert_eq!(
        descending,
        vec![
            three.clone(),
            equal[0].clone(),
            equal[1].clone(),
            one.clone(),
            unrated.clone()
        ],
        "the same tie-break holds in the natural descending order, with the unrated track last"
    );

    let explicit = page_at(
        &repository,
        0,
        500,
        TrackOrder::Rating,
        &TrackListFilter::new().with_sort_direction(Some(SortDirection::Descending)),
        Some(&user),
    )
    .await;
    assert_eq!(explicit, descending);
}

#[sqlx::test]
async fn canonical_id_tie_break_is_load_bearing(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut fixture = Vec::new();

    for index in 0..1_001u32 {
        let track = repository
            .get_or_create(
                source("local", &format!("tie-{index:04}.flac")),
                metadata(&format!("item{:02}", index % 10)).with_genres(vec!["Rock".to_owned()]),
            )
            .await
            .unwrap();

        fixture.push((format!("item{:02}", index % 10), track.id().clone()));
    }

    let mut by_title = fixture.clone();
    by_title.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_uuid().cmp(right.1.as_uuid()))
    });
    let by_title = by_title.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
    let mut by_id = fixture.iter().map(|(_, id)| id.clone()).collect::<Vec<_>>();
    by_id.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));

    for (order, expected) in [
        (TrackOrder::Title, by_title),
        (TrackOrder::Artist, by_id.clone()),
        (TrackOrder::Genre, by_id.clone()),
    ] {
        let mut totals = BTreeSet::new();

        for offset in [0u32, 500, 1_000] {
            let observed = repository
                .page_tracks(offset, 500, order, &TrackListFilter::new(), None)
                .await
                .unwrap();
            let expected_page = expected
                .iter()
                .skip(offset as usize)
                .take(500)
                .cloned()
                .collect::<Vec<_>>();

            assert_eq!(page_ids(&observed), expected_page, "{order:?} at {offset}");
            totals.insert(observed.total());
        }

        assert_eq!(
            totals,
            BTreeSet::from([1_001]),
            "the {order:?} total is the same at every offset"
        );
    }
}

#[sqlx::test]
async fn random_is_repeatable_per_seed_and_not_persistent(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    for index in 0..40u32 {
        repository
            .get_or_create(
                source("local", &format!("random-{index:03}.flac")),
                metadata(&format!("random {index:03}")),
            )
            .await
            .unwrap();
    }

    let before = sqlx::query_as::<_, (Uuid, String, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, title, updated_at FROM tracks ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let first = TrackListFilter::new().with_random_seed(Some(RandomSeed::new(4_242)));
    let filtered = TrackListFilter::new()
        .with_title(selector("random 00"))
        .with_random_seed(Some(RandomSeed::new(4_242)));
    let other_filtered = filtered
        .clone()
        .with_random_seed(Some(RandomSeed::new(9_999)));
    let second = TrackListFilter::new().with_random_seed(Some(RandomSeed::new(9_999)));
    let whole = page_at(&repository, 0, 500, TrackOrder::Random, &first, None).await;
    let other_seed = page_at(&repository, 0, 500, TrackOrder::Random, &second, None).await;

    assert_eq!(
        page_at(&repository, 0, 10, TrackOrder::Random, &first, None).await,
        page_at(&repository, 0, 10, TrackOrder::Random, &first, None).await,
        "the same seed repeats the same page"
    );
    assert_eq!(
        page_at(&repository, 10, 10, TrackOrder::Random, &first, None).await,
        page_at(&repository, 10, 10, TrackOrder::Random, &first, None).await,
        "and repeats a later page"
    );
    assert_eq!(
        total_of(&repository, TrackOrder::Random, &first, None).await,
        40,
        "random does not change membership"
    );
    assert_eq!(
        total_of(&repository, TrackOrder::Random, &second, None).await,
        40
    );
    assert_ne!(whole, other_seed, "a different seed is a different order");
    assert_eq!(
        whole
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<BTreeSet<_>>()
            .len(),
        40,
        "the random order is a permutation of the membership"
    );
    assert_eq!(
        whole.iter().take(10).cloned().collect::<Vec<_>>(),
        page_at(&repository, 0, 10, TrackOrder::Random, &first, None).await,
        "the first page is the head of the same permutation"
    );
    assert_eq!(
        whole.iter().skip(10).take(10).cloned().collect::<Vec<_>>(),
        page_at(&repository, 10, 10, TrackOrder::Random, &first, None).await,
        "the second page continues the same permutation"
    );

    let filtered_whole = page_at(&repository, 0, 500, TrackOrder::Random, &filtered, None).await;

    assert_eq!(filtered_whole.len(), 10, "the selector narrows membership");
    assert_eq!(
        total_of(&repository, TrackOrder::Random, &filtered, None).await,
        10
    );
    assert_eq!(
        page_at(&repository, 0, 2, TrackOrder::Random, &filtered, None).await,
        filtered_whole[..2].to_vec(),
        "a selector keeps the page a head of the same seeded permutation"
    );
    assert_eq!(
        page_at(&repository, 0, 2, TrackOrder::Random, &filtered, None).await,
        page_at(&repository, 0, 2, TrackOrder::Random, &filtered, None).await,
        "and repeats"
    );
    assert_ne!(
        filtered_whole,
        page_at(
            &repository,
            0,
            500,
            TrackOrder::Random,
            &other_filtered,
            None
        )
        .await,
        "a second seed permutes the filtered membership too"
    );

    let after = sqlx::query_as::<_, (Uuid, String, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, title, updated_at FROM tracks ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(before, after, "a random page writes no tracks row");
}

#[sqlx::test]
async fn date_added_is_the_owned_source_timestamp(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("added-user", "Added User")
        .await
        .unwrap();
    let owned = repository
        .get_or_create(source("local", "owned.flac"), metadata("owned"))
        .await
        .unwrap();
    let provider_only = repository
        .get_or_create(
            source("youtube_music", "vid-added"),
            metadata("provider only"),
        )
        .await
        .unwrap();

    let owned_id = owned.id().clone();
    let provider_only_id = provider_only.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&provider_only_id), true)
        .await
        .unwrap();
    age_track(&pool, &owned_id, -100).await;
    age_track(&pool, &provider_only_id, -1).await;
    age_source(&pool, &source("local", "owned.flac"), -200).await;

    assert_eq!(
        page_at(
            &repository,
            0,
            500,
            TrackOrder::DateAdded,
            &TrackListFilter::new(),
            Some(&user)
        )
        .await,
        vec![owned_id.clone(), provider_only_id.clone()],
        "date_added orders by the owned source timestamp and puts the provider-only track last"
    );
    assert_eq!(
        page_at(
            &repository,
            0,
            500,
            TrackOrder::RecentlyAdded,
            &TrackListFilter::new(),
            Some(&user)
        )
        .await,
        vec![provider_only_id, owned_id],
        "recently_added is the different materialisation fact"
    );
}

#[sqlx::test]
async fn selected_representation_is_absent_for_provider_only(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("representation-user", "Representation User")
        .await
        .unwrap();
    let provider_only = repository
        .get_or_create(
            source("youtube_music", "vid-representation"),
            metadata("provider only"),
        )
        .await
        .unwrap();
    let local = repository
        .get_or_create(
            source("local", "a-lowest.flac"),
            metadata("two local sources"),
        )
        .await
        .unwrap();

    let provider_only_id = provider_only.id().clone();
    let local_id = local.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&provider_only_id), true)
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source("local", "a-lowest.flac"),
            &local_facts(Some(6), Some(900)),
        )
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_sources (id, track_id, provider_id, source_scope, external_id, created_at) \
         VALUES ($1, $2, 'local', $3, 'b-higher.flac', CURRENT_TIMESTAMP)",
    )
    .bind(Uuid::now_v7())
    .bind(local_id.as_uuid())
    .bind(encoded(&source("local", "b-higher.flac")))
    .execute(&pool)
    .await
    .unwrap();
    repository
        .set_source_file_facts(
            &SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named(LOCAL_ROOT.to_owned()),
                "b-higher.flac".to_owned(),
            )
            .unwrap(),
            &local_facts(Some(2), Some(100)),
        )
        .await
        .unwrap();

    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_channels(Some(range(Some(6), Some(6)))),
            Some(&user)
        )
        .await,
        vec![local_id.clone()],
        "the tie is broken by the lowest external_id, a-lowest.flac"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_channels(Some(range(Some(2), Some(2)))),
            Some(&user)
        )
        .await,
        0,
        "b-higher.flac's channels are not the selected representation's"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_channels(Some(range(Some(1), Some(8)))),
            Some(&user)
        )
        .await,
        1,
        "a provider-only track matches no channels range"
    );
    assert_eq!(
        page_at(
            &repository,
            0,
            500,
            TrackOrder::Channels,
            &TrackListFilter::new(),
            Some(&user)
        )
        .await,
        vec![local_id.clone(), provider_only_id],
        "channels sorts the absent provider-only track last"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_bitrate(Some(range(Some(800), None))),
            Some(&user)
        )
        .await,
        1,
        "the selected representation's bitrate is 900"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_codec(selector("flac")),
            Some(&user)
        )
        .await,
        1
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_size(Some(range(Some(1), None))),
            Some(&user)
        )
        .await,
        1
    );
}

#[sqlx::test]
async fn missing_is_scoped_to_present_local_sources(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("missing-user", "Missing User")
        .await
        .unwrap();
    let provider_only = repository
        .get_or_create(
            source("youtube_music", "vid-missing"),
            metadata("provider only"),
        )
        .await
        .unwrap();
    let walked = repository
        .get_or_create(source("local", "walked.flac"), metadata("walked"))
        .await
        .unwrap();
    let gone = repository
        .get_or_create(source("local", "gone.flac"), metadata("gone"))
        .await
        .unwrap();

    let provider_only_id = provider_only.id().clone();
    let walked_id = walked.id().clone();
    let gone_id = gone.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&provider_only_id), true)
        .await
        .unwrap();
    repository
        .set_source_file_facts(&source("local", "walked.flac"), &local_facts(Some(2), None))
        .await
        .unwrap();
    repository
        .set_source_file_facts(&source("local", "gone.flac"), &local_facts(Some(2), None))
        .await
        .unwrap();
    set_missing(&pool, &source("local", "gone.flac"), true).await;

    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_missing(Some(true)),
            Some(&user)
        )
        .await,
        vec![gone_id.clone()],
        "missing=true is a local source with missing_at set"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_missing(Some(false)),
            Some(&user)
        )
        .await,
        sorted(&[&walked_id, &provider_only_id]),
        "missing=false is the provider-only and present-local tracks"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_missing(Some(false)),
            Some(&user)
        )
        .await,
        2
    );
}

#[sqlx::test]
async fn boolean_selector_defaults_follow_the_stored_fact(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("boolean-user", "Boolean User")
        .await
        .unwrap();
    let starred = repository
        .get_or_create(source("local", "starred.flac"), metadata("starred"))
        .await
        .unwrap();
    let plain = repository
        .get_or_create(source("local", "plain.flac"), metadata("plain"))
        .await
        .unwrap();
    let rated = repository
        .get_or_create(source("local", "rated.flac"), metadata("rated"))
        .await
        .unwrap();

    let starred_id = starred.id().clone();
    let plain_id = plain.id().clone();
    let rated_id = rated.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&starred_id), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(&rated_id), Some(1))
        .await
        .unwrap();

    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_starred(Some(true)),
            Some(&user)
        )
        .await,
        vec![starred_id.clone()],
        "starred=true is the favourited track"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_starred(Some(false)),
            Some(&user)
        )
        .await,
        sorted(&[&plain_id, &rated_id]),
        "starred=false includes a track with no track_favorites row"
    );
    assert_eq!(
        total(&repository, &TrackListFilter::new(), Some(&user)).await,
        3,
        "an omitted boolean returns both"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_has_rating(Some(true)),
            Some(&user)
        )
        .await,
        vec![rated_id.clone()],
        "a rating of 1 is present"
    );
    assert_eq!(
        list(
            &repository,
            &TrackListFilter::new().with_has_rating(Some(false)),
            Some(&user)
        )
        .await,
        sorted(&[&starred_id, &plain_id])
    );
}

#[sqlx::test]
async fn anonymous_state_selectors_are_not_all_users(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("anonymous-user", "Anonymous User")
        .await
        .unwrap();
    let owned = repository
        .get_or_create(source("local", "owned.flac"), metadata("owned"))
        .await
        .unwrap();
    let also_owned = repository
        .get_or_create(source("local", "also-owned.flac"), metadata("also owned"))
        .await
        .unwrap();

    let owned_id = owned.id().clone();
    let also_owned_id = also_owned.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&owned_id), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(&owned_id), Some(5))
        .await
        .unwrap();

    let scoped = TrackListFilter::new().with_scope(TrackListScope::Owned);
    let scope_total = total(&repository, &scoped, None).await;

    assert_eq!(scope_total, 2);
    assert_eq!(
        total(&repository, &scoped.clone().with_starred(Some(true)), None).await,
        0,
        "starred=true with no principal returns nothing"
    );
    assert_eq!(
        total(
            &repository,
            &scoped.clone().with_rating(Some(range(Some(1), Some(5)))),
            None
        )
        .await,
        0,
        "a rating selector with no principal returns nothing"
    );
    assert_eq!(
        list(&repository, &scoped.clone().with_starred(Some(false)), None).await,
        sorted(&[&owned_id, &also_owned_id]),
        "starred=false is the scope's own list, not an empty one"
    );
    assert_eq!(
        total(&repository, &scoped.clone().with_starred(Some(false)), None).await,
        scope_total,
        "the total is the scope's own count"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_starred(Some(true)),
            None
        )
        .await,
        0
    );
}

#[sqlx::test]
async fn selectors_do_not_broaden_scope(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("scope-user", "Scope User")
        .await
        .unwrap();
    let owned = repository
        .get_or_create(source("local", "owned.flac"), metadata("owned song"))
        .await
        .unwrap();
    let provider_only = repository
        .get_or_create(
            source("youtube_music", "vid-scope"),
            metadata("remote only"),
        )
        .await
        .unwrap();

    let owned_id = owned.id().clone();
    let provider_only_id = provider_only.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&provider_only_id), true)
        .await
        .unwrap();

    let star = TrackListFilter::new().with_starred(Some(true));
    assert_eq!(
        list(&repository, &star, Some(&user)).await,
        vec![provider_only_id],
        "a favourited provider-only track is engaged"
    );

    let owned_star = star.clone().with_scope(TrackListScope::Owned);
    assert!(
        list(&repository, &owned_star, Some(&user)).await.is_empty(),
        "and is absent under owned for the same selector"
    );
    assert_eq!(total(&repository, &owned_star, Some(&user)).await, 0);
    assert_eq!(total(&repository, &star, Some(&user)).await, 1);

    let title = TrackListFilter::new().with_title(selector("song"));
    assert_eq!(
        list(&repository, &title, Some(&user)).await,
        vec![owned_id],
        "a title selector does not broaden the scope to the provider-only track"
    );
    assert_eq!(total(&repository, &title, Some(&user)).await, 1);

    let owned_title = title.with_scope(TrackListScope::Owned);
    assert_eq!(total(&repository, &owned_title, Some(&user)).await, 1);
}

#[sqlx::test]
async fn page_projection_carries_membership_state_and_sources_on_a_deep_page(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("deep-user", "Deep User")
        .await
        .unwrap();
    let deep_album = repository
        .resolve_or_create_album(&album("Deep").with_album_artist(credit("Deep Artist", None)))
        .await
        .unwrap();

    for index in 0..1_000u32 {
        repository
            .get_or_create(
                source("local", &format!("deep-{index:04}.flac")),
                metadata(&format!("item{index:04}")),
            )
            .await
            .unwrap();
    }

    let placed = repository
        .get_or_create(source("local", "deep-placed.flac"), metadata("zzz placed"))
        .await
        .unwrap();
    let placed_id = placed.id().clone();

    repository
        .set_album_track(
            deep_album.id(),
            &placed_id,
            &AlbumTrack::new(Some(2), Some(3), 2)
                .unwrap()
                .with_totals(Some(2), Some(3)),
        )
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(&placed_id), Some(5))
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Track(&placed_id), true)
        .await
        .unwrap();
    seed_play(&pool, user, &placed_id, chrono::Utc::now()).await;

    let observed = repository
        .page_tracks(
            1_000,
            1,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();

    assert_eq!(observed.total(), 1_001);
    assert_eq!(observed.items().len(), 1);

    let item = &observed.items()[0];

    assert_eq!(item.track().id(), &placed_id);
    assert_eq!(item.album().map(|album| album.id()), Some(deep_album.id()));
    assert_eq!(
        item.album_artist().map(str::to_owned),
        Some("Deep Artist".to_owned())
    );

    let membership = item.membership().expect("the deep item carries membership");

    assert_eq!(membership.disc_number(), Some(2));
    assert_eq!(membership.track_number(), Some(3));
    assert_eq!(membership.disc_total(), Some(2));
    assert_eq!(membership.track_total(), Some(3));
    assert_eq!(item.state().rating(), Some(5));
    assert!(item.state().is_starred());
    assert_eq!(item.state().play_count(), 1);
    assert_eq!(item.sources().len(), 1);
    assert_eq!(item.sources()[0].provider_id().as_str(), "local");
}

#[sqlx::test]
async fn page_reads_do_not_follow_the_page_size(pool: PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let repository = PostgresTrackRepository::new(pool.clone());

    let mut first_id = None;

    for index in 0..20 {
        let track = repository
            .get_or_create(
                source("local", &format!("batched-{index:02}.flac")),
                metadata(&format!("batched {index:02}"))
                    .with_genres(vec!["Rock".to_owned()])
                    .with_credits(vec![credit("Player", Some(CreditRole::Performer))]),
            )
            .await
            .unwrap();

        if first_id.is_none() {
            first_id = Some(track.id().clone());
        }

        repository
            .set_source_file_facts(
                &source("local", &format!("batched-{index:02}.flac")),
                &local_facts(Some(2), Some(320)),
            )
            .await
            .unwrap();
    }

    let strip = TrackListFilter::new().with_title(selector("batched"));
    let narrow = strip
        .clone()
        .with_genre(Some(text_set(&["rock"])))
        .with_role(Some(role_set(&[RoleSelector::Performer])))
        .with_channels(Some(range(Some(1), Some(8))));
    let user = provision(pool.clone())
        .create_user("read-user", "Read User")
        .await
        .unwrap();
    let first = repository
        .find_track(first_id.as_ref().unwrap())
        .await
        .unwrap()
        .unwrap();
    seed_play(&pool, user, first.id(), chrono::Utc::now()).await;

    let mut measured = Vec::new();

    for limit in [2u32, 20] {
        let genres = settled_table_scans(&pool, "track_genres").await;
        let credits = settled_table_scans(&pool, "track_artist_credits").await;
        let sources = settled_table_scans(&pool, "track_sources").await;
        let plays = settled_table_scans(&pool, "track_play_stats").await;
        let observed = repository
            .page_tracks(0, limit, TrackOrder::Title, &strip, Some(user))
            .await
            .unwrap();

        measured.push((
            limit,
            observed.items().len(),
            observed.total(),
            settled_table_scans(&pool, "track_genres").await - genres,
            settled_table_scans(&pool, "track_artist_credits").await - credits,
            settled_table_scans(&pool, "track_sources").await - sources,
            settled_table_scans(&pool, "track_play_stats").await - plays,
        ));
    }

    assert_eq!(measured[0].1, 2);
    assert_eq!(measured[0].2, 20);
    assert_eq!(measured[1].1, 20);
    assert_eq!(measured[1].2, 20);
    assert_eq!(
        (measured[1].3, measured[1].4, measured[1].5, measured[1].6),
        (measured[0].3, measured[0].4, measured[0].5, measured[0].6),
        "a page of 20 must not read the batch sets more often than a page of 2"
    );
    assert_eq!(
        measured[0].3, 1,
        "the genre set is read by exactly one statement"
    );
    assert_eq!(
        measured[0].4, 1,
        "the credits are read by exactly one statement"
    );
    assert_eq!(
        measured[0].6, 1,
        "the principal state is read by exactly one statement"
    );

    let mut filtered = Vec::new();

    for limit in [2u32, 20] {
        let plays = settled_table_scans(&pool, "track_play_stats").await;
        let observed = repository
            .page_tracks(0, limit, TrackOrder::Title, &narrow, Some(user))
            .await
            .unwrap();

        filtered.push((
            observed.items().len(),
            observed.total(),
            settled_table_scans(&pool, "track_play_stats").await - plays,
        ));
    }

    assert_eq!(filtered[0].0, 2);
    assert_eq!(filtered[0].1, 20);
    assert_eq!(filtered[1].0, 20);
    assert_eq!(filtered[1].1, 20);
    assert_eq!(
        filtered[1].2, filtered[0].2,
        "a selector-heavy page of 20 reads the principal state once, like a page of 2"
    );
}

#[test]
fn unknown_values_and_bad_ranges_are_explicit_errors() {
    assert_eq!(TrackOrder::parse("bpm"), None);
    assert_eq!(TrackOrder::parse("comment"), None);
    assert_eq!(TrackOrder::parse("explicitStatus"), None);
    assert_eq!(RoleSelector::parse("other"), None);
    assert_eq!(SortDirection::parse("sideways"), None);
    assert!(matches!(
        Range::new(Some(5), Some(1)),
        Err(ValidationError::Invalid { field: "range" })
    ));
    assert!(matches!(
        Range::new(None::<i32>, None),
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
        TrackListFilter::new()
            .with_random_seed(Some(RandomSeed::new(1)))
            .resolved_seed(TrackOrder::Title),
        Err(ValidationError::Invalid {
            field: "random_seed"
        })
    );
}

#[sqlx::test]
async fn selected_source_facts_are_not_mixed(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let track = repository
        .get_or_create(source("local", "a-selected.flac"), metadata("mixed facts"))
        .await
        .unwrap();
    let id = track.id().clone();

    add_local_source(&pool, &id, "b-other.flac").await;

    let mut selected = local_facts(Some(2), Some(100));
    selected.content_type = None;
    selected.codec = Some("aac".to_owned());
    selected.size_bytes = 0;
    repository
        .set_source_file_facts(&source("local", "a-selected.flac"), &selected)
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source("local", "b-other.flac"),
            &local_facts(Some(6), Some(900)),
        )
        .await
        .unwrap();

    let expected = SourceRepresentationFacts {
        source: source("local", "a-selected.flac"),
        content_type: None,
        suffix: Some("flac".to_owned()),
        container: Some("flac".to_owned()),
        codec: Some("aac".to_owned()),
        size_bytes: Some(0),
        bitrate_kbps: Some(100),
        sample_rate_hz: Some(44_100),
        bit_depth: Some(16),
        channels: Some(2),
    };

    let page = repository
        .page_tracks(0, 500, TrackOrder::Id, &TrackListFilter::new(), None)
        .await
        .unwrap();

    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].representation(), Some(&expected));

    let views = repository.track_views(&[id], None).await.unwrap();

    assert_eq!(views.len(), 1);
    assert_eq!(views[0].representation(), Some(&expected));
}

#[sqlx::test]
async fn selected_source_facts_break_equal_external_ids_by_scope(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let elsewhere = source_in("local", OTHER_ROOT, "same.flac");
    let track = repository
        .get_or_create(elsewhere.clone(), metadata("scoped tie"))
        .await
        .unwrap();
    let id = track.id().clone();

    add_local_source(&pool, &id, "same.flac").await;

    repository
        .set_source_file_facts(&elsewhere, &local_facts(Some(2), Some(100)))
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source_in("local", LOCAL_ROOT, "same.flac"),
            &local_facts(Some(6), Some(900)),
        )
        .await
        .unwrap();

    let page = repository
        .page_tracks(0, 500, TrackOrder::Id, &TrackListFilter::new(), None)
        .await
        .unwrap();

    assert_eq!(page.items().len(), 1);
    let representation = page.items()[0].representation().unwrap();

    assert_eq!(representation.source, elsewhere);
    assert_eq!(representation.channels, Some(2));
    assert_eq!(representation.bitrate_kbps, Some(100));
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_channels(Some(range(Some(2), Some(2)))),
            None
        )
        .await,
        1,
        "the selector picks the same scope the projection does"
    );
    assert_eq!(
        total(
            &repository,
            &TrackListFilter::new().with_channels(Some(range(Some(6), Some(6)))),
            None
        )
        .await,
        0,
        "the losing scope's channels are not the selected representation's"
    );
}

#[sqlx::test]
async fn provider_only_projection_has_no_representation(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("provider-only-facts", "Provider Only Facts")
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            source("youtube_music", "vid-facts"),
            metadata("provider only facts"),
        )
        .await
        .unwrap();
    let id = track.id().clone();

    repository
        .set_starred(user, StateTarget::Track(&id), true)
        .await
        .unwrap();

    let page = repository
        .page_tracks(0, 500, TrackOrder::Id, &TrackListFilter::new(), Some(user))
        .await
        .unwrap();

    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].track().id(), &id);
    assert_eq!(page.items()[0].representation(), None);

    let views = repository.track_views(&[id], Some(user)).await.unwrap();

    assert_eq!(views.len(), 1);
    assert_eq!(views[0].representation(), None);
}

#[sqlx::test]
async fn local_projection_preserves_nullable_and_zero_size(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let zero = repository
        .get_or_create(source("local", "zero-size.flac"), metadata("zero size"))
        .await
        .unwrap();
    let absent = repository
        .get_or_create(source("local", "absent-size.flac"), metadata("absent size"))
        .await
        .unwrap();
    let zero_id = zero.id().clone();
    let absent_id = absent.id().clone();

    let mut zero_facts = local_facts(Some(2), Some(100));
    zero_facts.size_bytes = 0;
    repository
        .set_source_file_facts(&source("local", "zero-size.flac"), &zero_facts)
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source("local", "absent-size.flac"),
            &local_facts(Some(2), Some(100)),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE track_sources SET size_bytes = NULL \
         WHERE provider_id = 'local' AND source_scope = $1 AND external_id = 'absent-size.flac'",
    )
    .bind(encoded(&source("local", "absent-size.flac")))
    .execute(&pool)
    .await
    .unwrap();

    let views = repository
        .track_views(&[zero_id.clone(), absent_id.clone()], None)
        .await
        .unwrap();
    let zero_view = views
        .iter()
        .find(|view| view.track().id() == &zero_id)
        .unwrap();
    let absent_view = views
        .iter()
        .find(|view| view.track().id() == &absent_id)
        .unwrap();

    assert_eq!(zero_view.representation().unwrap().size_bytes, Some(0));
    assert_eq!(absent_view.representation().unwrap().size_bytes, None);
}

#[sqlx::test]
async fn track_projection_preserves_order_and_repeats(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .get_or_create(source("local", "order-first.flac"), metadata("order first"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            source("local", "order-second.flac"),
            metadata("order second"),
        )
        .await
        .unwrap();
    let third = repository
        .get_or_create(source("local", "order-third.flac"), metadata("order third"))
        .await
        .unwrap();
    let first_id = first.id().clone();
    let second_id = second.id().clone();
    let third_id = third.id().clone();

    let requested = vec![
        third_id.clone(),
        first_id.clone(),
        third_id.clone(),
        TrackId::new(),
        second_id.clone(),
    ];
    let views = repository.track_views(&requested, None).await.unwrap();
    let ids = views
        .iter()
        .map(|view| view.track().id().clone())
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        vec![third_id.clone(), first_id.clone(), third_id, second_id]
    );
    assert!(repository.track_views(&[], None).await.unwrap().is_empty());
}

#[sqlx::test]
async fn album_projection_preserves_its_membership_context(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_album = repository
        .resolve_or_create_album(
            &album("Context A").with_album_artist(credit("Context Artist", None)),
        )
        .await
        .unwrap();
    let second_album = repository
        .resolve_or_create_album(
            &album("Context B").with_album_artist(credit("Context Artist", None)),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(source("local", "context.flac"), metadata("context"))
        .await
        .unwrap();
    let id = track.id().clone();

    repository
        .set_source_file_facts(
            &source("local", "context.flac"),
            &local_facts(Some(2), Some(320)),
        )
        .await
        .unwrap();
    attach_album_track(&pool, first_album.id(), &id, 1, 2, 1).await;
    attach_album_track(&pool, second_album.id(), &id, 2, 7, 5).await;

    let first_entries = repository
        .find_album_entries(first_album.id(), None)
        .await
        .unwrap();
    let second_entries = repository
        .find_album_entries(second_album.id(), None)
        .await
        .unwrap();

    assert_eq!(first_entries.len(), 1);
    assert_eq!(second_entries.len(), 1);
    assert_eq!(first_entries[0].membership().disc_number(), Some(1));
    assert_eq!(first_entries[0].membership().track_number(), Some(2));
    assert_eq!(second_entries[0].membership().disc_number(), Some(2));
    assert_eq!(second_entries[0].membership().track_number(), Some(7));
    assert_eq!(first_entries[0].representation().unwrap().channels, Some(2));
    assert_eq!(
        second_entries[0].representation().unwrap().channels,
        Some(2)
    );
}

#[sqlx::test]
async fn invalid_representation_numeric_data_fails(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let track = repository
        .get_or_create(
            source("local", "invalid-channels.flac"),
            metadata("invalid channels"),
        )
        .await
        .unwrap();
    let id = track.id().clone();

    repository
        .set_source_file_facts(
            &source("local", "invalid-channels.flac"),
            &local_facts(Some(2), Some(320)),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE track_sources SET channels = 300 \
         WHERE provider_id = 'local' AND source_scope = $1 AND external_id = 'invalid-channels.flac'",
    )
    .bind(encoded(&source("local", "invalid-channels.flac")))
    .execute(&pool)
    .await
    .unwrap();

    assert!(matches!(
        repository
            .page_tracks(0, 500, TrackOrder::Id, &TrackListFilter::new(), None)
            .await,
        Err(TrackRepositoryError::InvalidData)
    ));
    assert!(matches!(
        repository.track_views(&[id], None).await,
        Err(TrackRepositoryError::InvalidData)
    ));
}

#[sqlx::test]
async fn album_track_page_preserves_unengaged_provider_members(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(
            &album("Provider Members").with_album_artist(credit("Narrator", None)),
        )
        .await
        .unwrap();
    let owned = repository
        .get_or_create(
            source("local", "member-owned.flac"),
            metadata("owned member"),
        )
        .await
        .unwrap();
    let provider_only = repository
        .get_or_create(
            source("youtube_music", "vid-member"),
            metadata("provider member"),
        )
        .await
        .unwrap();
    let owned_id = owned.id().clone();
    let provider_id = provider_only.id().clone();

    repository
        .set_album_track(
            album.id(),
            &owned_id,
            &AlbumTrack::new(Some(1), Some(1), 1).unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            &provider_id,
            &AlbumTrack::new(Some(1), Some(2), 2).unwrap(),
        )
        .await
        .unwrap();

    let contextual = TrackListFilter::new().with_album_id(Some(album_id_set(&[album.id()])));

    assert_eq!(
        list(&repository, &contextual, None).await,
        vec![owned_id.clone()],
        "a generic engaged page excludes the unengaged provider member"
    );
    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            0,
            500,
            TrackOrder::Id,
            &TrackListFilter::new(),
            None,
        )
        .await,
        sorted(&[&owned_id, &provider_id]),
        "the explicit album page carries every recorded member without provider activity"
    );
    assert_eq!(
        album_total(&repository, album.id(), &TrackListFilter::new(), None).await,
        2
    );
}

#[sqlx::test]
async fn album_track_page_uses_requested_membership_for_projection_filter_and_order(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let requested = repository
        .resolve_or_create_album(
            &album("Bravo")
                .with_album_artist(credit("Anna", None))
                .with_year(Some(2020))
                .unwrap(),
        )
        .await
        .unwrap();
    let first_primary = repository
        .resolve_or_create_album(
            &album("Alpha")
                .with_album_artist(credit("Zed", None))
                .with_year(Some(1990))
                .unwrap(),
        )
        .await
        .unwrap();
    let second_primary = repository
        .resolve_or_create_album(
            &album("Aardvark")
                .with_album_artist(credit("Aaa", None))
                .with_year(Some(1985))
                .unwrap(),
        )
        .await
        .unwrap();
    let first = repository
        .get_or_create(
            source("local", "context-first.flac"),
            metadata("context first"),
        )
        .await
        .unwrap();
    let second = repository
        .get_or_create(
            source("local", "context-second.flac"),
            metadata("context second"),
        )
        .await
        .unwrap();
    let first_id = first.id().clone();
    let second_id = second.id().clone();

    repository
        .set_album_track(
            first_primary.id(),
            &first_id,
            &AlbumTrack::new(Some(1), Some(1), 1).unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            second_primary.id(),
            &second_id,
            &AlbumTrack::new(Some(2), Some(2), 2).unwrap(),
        )
        .await
        .unwrap();
    attach_album_track(&pool, requested.id(), &first_id, 2, 2, 2).await;
    attach_album_track(&pool, requested.id(), &second_id, 1, 1, 1).await;

    let all = TrackListFilter::new();
    let page = repository
        .page_album_tracks(requested.id(), 0, 500, TrackOrder::Track, &all, None)
        .await
        .unwrap();

    assert_eq!(page.items().len(), 2);
    assert_eq!(page.items()[0].track().id(), &second_id);
    assert_eq!(
        page.items()[0].album().map(|album| album.title()),
        Some("Bravo")
    );
    assert_eq!(
        page.items()[0].album().and_then(|album| album.year()),
        Some(2020)
    );
    assert_eq!(page.items()[0].album_artist(), Some("Anna"));
    assert_eq!(page.items()[0].membership().unwrap().disc_number(), Some(1));
    assert_eq!(
        page.items()[0].membership().unwrap().track_number(),
        Some(1)
    );
    assert_eq!(page.items()[1].track().id(), &first_id);
    assert_eq!(
        page.items()[1].album().map(|album| album.title()),
        Some("Bravo")
    );
    assert_eq!(page.items()[1].album_artist(), Some("Anna"));
    assert_eq!(page.items()[1].membership().unwrap().disc_number(), Some(2));
    assert_eq!(
        page.items()[1].membership().unwrap().track_number(),
        Some(2)
    );

    let descending = all
        .clone()
        .with_sort_direction(Some(SortDirection::Descending));

    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Track,
            &all,
            None,
        )
        .await,
        vec![second_id.clone(), first_id.clone()],
        "track order reads the requested membership"
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Track,
            &descending,
            None,
        )
        .await,
        vec![first_id.clone(), second_id.clone()]
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Album,
            &all,
            None,
        )
        .await,
        vec![second_id.clone(), first_id.clone()]
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Album,
            &descending,
            None,
        )
        .await,
        vec![first_id.clone(), second_id.clone()]
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::AlbumArtist,
            &all,
            None,
        )
        .await,
        vec![first_id.clone(), second_id.clone()],
        "album-artist order reads the requested album's artist, not the primary one"
    );

    let in_range = all.clone().with_year(Some(range(Some(2015), Some(2025))));
    let outside_range = all.clone().with_year(Some(range(Some(1980), Some(1999))));

    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Id,
            &in_range,
            None,
        )
        .await,
        sorted(&[&first_id, &second_id]),
        "the year selector reads the requested album's year"
    );
    assert!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Id,
            &outside_range,
            None,
        )
        .await
        .is_empty(),
        "primary years do not leak into the requested album page"
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Id,
            &all.clone().with_track_number(Some(range(Some(1), Some(1)))),
            None,
        )
        .await,
        vec![second_id.clone()]
    );
    assert_eq!(
        album_page_at(
            &repository,
            requested.id(),
            0,
            500,
            TrackOrder::Id,
            &all.clone().with_disc_number(Some(range(Some(2), Some(2)))),
            None,
        )
        .await,
        vec![first_id.clone()]
    );
}

#[sqlx::test]
async fn album_artist_role_respects_explicit_album_context(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let primary = repository
        .resolve_or_create_album(&album("Alpha").with_album_artist(credit("Zed", None)))
        .await
        .unwrap();
    let requested = repository
        .resolve_or_create_album(&album("Bravo").with_album_artist(credit("Anna", None)))
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            source("local", "album-role-context.flac"),
            metadata("context"),
        )
        .await
        .unwrap();
    let track_id = track.id().clone();
    let zed = album_artist_of(&pool, primary.id()).await;
    let anna = album_artist_of(&pool, requested.id()).await;

    repository
        .set_album_track(primary.id(), &track_id, &AlbumTrack::unplaced())
        .await
        .unwrap();
    attach_album_track(&pool, requested.id(), &track_id, 1, 1, 0).await;

    let album_artist_role =
        TrackListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist])));
    let zed_filter = album_artist_role
        .clone()
        .with_artists_id(Some(artist_id_set(&[&zed])));
    let anna_filter = album_artist_role.with_artists_id(Some(artist_id_set(&[&anna])));
    let zed_page = repository
        .page_album_tracks(requested.id(), 0, 500, TrackOrder::Id, &zed_filter, None)
        .await
        .unwrap();
    let anna_page = repository
        .page_album_tracks(requested.id(), 0, 500, TrackOrder::Id, &anna_filter, None)
        .await
        .unwrap();

    assert!(zed_page.items().is_empty());
    assert_eq!(zed_page.total(), 0);
    assert_eq!(page_ids(&anna_page), vec![track_id.clone()]);
    assert_eq!(anna_page.total(), 1);
    assert_eq!(
        list(&repository, &zed_filter, None).await,
        vec![track_id],
        "ordinary pages retain any-album membership semantics"
    );
}

#[sqlx::test]
async fn album_track_page_applies_filters_before_window_and_reports_total(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("album-page-user", "Album Page User")
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&album("Windowed").with_album_artist(credit("Narrator", None)))
        .await
        .unwrap();
    let mut ids = Vec::new();

    for index in 0..5u32 {
        let track = repository
            .get_or_create(
                source("local", &format!("window-{index}.flac")),
                metadata(&format!("window {index}")).with_genres(vec![if index == 2 {
                    "Pop".to_owned()
                } else {
                    "Rock".to_owned()
                }]),
            )
            .await
            .unwrap();
        let id = track.id().clone();

        repository
            .set_album_track(
                album.id(),
                &id,
                &AlbumTrack::new(Some(1), Some(u16::try_from(index).unwrap() + 1), index + 1)
                    .unwrap(),
            )
            .await
            .unwrap();
        ids.push(id);
    }

    set_missing(&pool, &source("local", "window-1.flac"), true).await;
    repository
        .set_starred(user, StateTarget::Track(&ids[0]), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(&ids[2]), Some(4))
        .await
        .unwrap();

    let all = TrackListFilter::new();

    assert_eq!(album_total(&repository, album.id(), &all, None).await, 5);
    assert_eq!(
        album_page_at(&repository, album.id(), 0, 2, TrackOrder::Track, &all, None,).await,
        ids[0..2].to_vec()
    );
    assert_eq!(
        album_page_at(&repository, album.id(), 2, 2, TrackOrder::Track, &all, None,).await,
        ids[2..4].to_vec()
    );
    assert_eq!(
        album_page_at(&repository, album.id(), 4, 2, TrackOrder::Track, &all, None,).await,
        vec![ids[4].clone()]
    );
    assert!(
        album_page_at(
            &repository,
            album.id(),
            40,
            2,
            TrackOrder::Track,
            &all,
            None,
        )
        .await
        .is_empty(),
        "a deep window past the album is empty"
    );

    let without_missing = all.clone().with_missing(Some(false));

    assert_eq!(
        album_total(&repository, album.id(), &without_missing, None).await,
        4
    );
    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            0,
            3,
            TrackOrder::Track,
            &without_missing,
            None,
        )
        .await,
        vec![ids[0].clone(), ids[2].clone(), ids[3].clone()],
        "the filter is applied before the window"
    );
    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            3,
            3,
            TrackOrder::Track,
            &without_missing,
            None,
        )
        .await,
        vec![ids[4].clone()]
    );
    assert!(
        album_page_at(
            &repository,
            album.id(),
            9,
            3,
            TrackOrder::Track,
            &without_missing,
            None,
        )
        .await
        .is_empty()
    );
    assert_eq!(
        album_total(&repository, album.id(), &without_missing, None).await,
        4,
        "the filtered total is reported for an empty window"
    );

    let only_missing = all.clone().with_missing(Some(true));

    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            0,
            500,
            TrackOrder::Id,
            &only_missing,
            None,
        )
        .await,
        vec![ids[1].clone()]
    );
    assert_eq!(
        album_total(&repository, album.id(), &only_missing, None).await,
        1
    );

    let rock = all.clone().with_genre(Some(text_set(&["rock"])));

    assert_eq!(
        album_page_at(&repository, album.id(), 0, 500, TrackOrder::Id, &rock, None,).await,
        sorted(&[&ids[0], &ids[1], &ids[3], &ids[4]])
    );

    let starred = all.clone().with_starred(Some(true));

    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            0,
            500,
            TrackOrder::Id,
            &starred,
            Some(&user),
        )
        .await,
        vec![ids[0].clone()]
    );

    let rated = all.with_rating(Some(range(Some(4), Some(4))));

    assert_eq!(
        album_page_at(
            &repository,
            album.id(),
            0,
            500,
            TrackOrder::Id,
            &rated,
            Some(&user),
        )
        .await,
        vec![ids[2].clone()]
    );

    let tied = album_page_at(
        &repository,
        album.id(),
        0,
        3,
        TrackOrder::Genre,
        &TrackListFilter::new(),
        None,
    )
    .await;
    let tied_next = album_page_at(
        &repository,
        album.id(),
        3,
        3,
        TrackOrder::Genre,
        &TrackListFilter::new(),
        None,
    )
    .await;
    let mut tied = [tied, tied_next].concat();

    tied.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    assert_eq!(
        tied,
        sorted(&ids.iter().collect::<Vec<_>>()),
        "genre ties page without duplicates or gaps"
    );
}

#[sqlx::test]
async fn album_track_page_has_no_per_row_projection_reads(pool: PgPool) {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&album("Batched").with_album_artist(credit("Narrator", None)))
        .await
        .unwrap();

    for index in 0..8u32 {
        let track = repository
            .get_or_create(
                source("local", &format!("album-batch-{index}.flac")),
                metadata(&format!("album batch {index}"))
                    .with_genres(vec!["Rock".to_owned()])
                    .with_credits(vec![credit("Player", Some(CreditRole::Performer))]),
            )
            .await
            .unwrap();
        let id = track.id().clone();

        repository
            .set_album_track(
                album.id(),
                &id,
                &AlbumTrack::new(Some(1), Some(u16::try_from(index).unwrap() + 1), index + 1)
                    .unwrap(),
            )
            .await
            .unwrap();
    }

    let filter = TrackListFilter::new();
    let mut measured = Vec::new();

    for limit in [2u32, 8] {
        let genres = settled_table_scans(&pool, "track_genres").await;
        let credits = settled_table_scans(&pool, "track_artist_credits").await;
        let sources = settled_table_scans(&pool, "track_sources").await;
        let memberships = settled_table_scans(&pool, "album_tracks").await;
        let observed = repository
            .page_album_tracks(album.id(), 0, limit, TrackOrder::Track, &filter, None)
            .await
            .unwrap();

        measured.push((
            limit,
            observed.items().len(),
            settled_table_scans(&pool, "track_genres").await - genres,
            settled_table_scans(&pool, "track_artist_credits").await - credits,
            settled_table_scans(&pool, "track_sources").await - sources,
            settled_table_scans(&pool, "album_tracks").await - memberships,
        ));
    }

    assert_eq!(measured[0].1, 2);
    assert_eq!(measured[1].1, 8);
    assert_eq!(
        (measured[1].2, measured[1].3, measured[1].4, measured[1].5),
        (measured[0].2, measured[0].3, measured[0].4, measured[0].5),
        "an explicit album page reads its batch sets once regardless of page size"
    );
}
