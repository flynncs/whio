use std::collections::BTreeSet;

use sqlx::PgPool;
use uuid::Uuid;

use super::PostgresTrackRepository;
use crate::features::identity::provision;
use crate::features::music_library::{
    domain::{
        AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, ArtistId, ArtistListFilter, ArtistSort,
        CreditRole, IdSet, ProviderId, RoleSelector, RoleSet, SelectorText, SortDirection,
        SourceIdentity, SourceScope, StateTarget, TrackId, TrackMetadata, ValidationError,
    },
    library::LibraryId,
    storage::contracts::{CatalogueWrite, LibraryRead, StateStore},
};

const LOCAL_ROOT: &str = "/music";
const OTHER_ROOT: &str = "/elsewhere";
const UNWALKED_ROOT: &str = "/unwalked";

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

fn primary(title: &str, artists: &[&str]) -> TrackMetadata {
    TrackMetadata::from_names(
        title.to_owned(),
        artists.iter().map(|name| (*name).to_owned()).collect(),
        Some(1_000),
    )
    .unwrap()
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

fn selector(value: &str) -> Option<SelectorText> {
    SelectorText::new(value).unwrap()
}

fn role_set(values: &[RoleSelector]) -> RoleSet {
    RoleSet::new(values.to_vec()).unwrap()
}

fn artist_set(ids: &[&ArtistId]) -> BTreeSet<Uuid> {
    ids.iter().map(|id| *id.as_uuid()).collect()
}

fn sorted(ids: &[&ArtistId]) -> Vec<ArtistId> {
    let mut ids = ids.iter().map(|id| (*id).clone()).collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    ids
}

async fn listed(
    repository: &PostgresTrackRepository,
    sort: ArtistSort,
    filter: &ArtistListFilter,
    user: Option<&Uuid>,
) -> Vec<ArtistId> {
    listed_at(repository, 0, 500, sort, filter, user).await
}

async fn listed_at(
    repository: &PostgresTrackRepository,
    offset: u32,
    limit: u32,
    sort: ArtistSort,
    filter: &ArtistListFilter,
    user: Option<&Uuid>,
) -> Vec<ArtistId> {
    repository
        .list_artists(offset, limit, sort, filter, user.copied())
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.artist().id().clone())
        .collect()
}

async fn counted(
    repository: &PostgresTrackRepository,
    sort: ArtistSort,
    filter: &ArtistListFilter,
    user: Option<&Uuid>,
) -> Vec<(ArtistId, u32, u32)> {
    repository
        .list_artists(0, 500, sort, filter, user.copied())
        .await
        .unwrap()
        .into_iter()
        .map(|summary| {
            (
                summary.artist().id().clone(),
                summary.album_count(),
                summary.song_count(),
            )
        })
        .collect()
}

async fn total(
    repository: &PostgresTrackRepository,
    filter: &ArtistListFilter,
    user: Option<&Uuid>,
) -> u32 {
    repository
        .count_artists(user.copied(), filter)
        .await
        .unwrap()
}

async fn named(repository: &PostgresTrackRepository, name: &str) -> ArtistId {
    repository
        .resolve_or_create_artist(&credit(name, None))
        .await
        .unwrap()
        .id()
        .clone()
}

async fn place(repository: &PostgresTrackRepository, album_row: &AlbumId, track: &TrackId) {
    repository
        .set_album_track(album_row, track, &AlbumTrack::unplaced())
        .await
        .unwrap();
}

async fn engaged_artist(
    repository: &PostgresTrackRepository,
    name: &str,
    external: &str,
) -> (ArtistId, AlbumId, TrackId) {
    let artist = named(repository, name).await;
    let album_row = repository
        .resolve_or_create_album(
            &album(&format!("{name} album")).with_album_artist(credit(name, None)),
        )
        .await
        .unwrap();
    let track = repository
        .get_or_create(source("local", external), primary(external, &[name]))
        .await
        .unwrap();

    place(repository, album_row.id(), track.id()).await;

    (artist, album_row.id().clone(), track.id().clone())
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

async fn age_artist_favorite(pool: &PgPool, artist: &ArtistId, user: Uuid, days: i64) {
    sqlx::query(
        "UPDATE artist_favorites SET starred_at = $3 WHERE artist_id = $1 AND user_id = $2",
    )
    .bind(artist.as_uuid())
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

async fn count_distinct_tracks(pool: &PgPool, artist: &ArtistId, role: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(DISTINCT credit.track_id) FROM track_artist_credits AS credit \
         WHERE credit.artist_id = $1 \
           AND (($2::TEXT IS NULL AND credit.role IS NULL) OR credit.role = $2)",
    )
    .bind(artist.as_uuid())
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn count_distinct_albums(pool: &PgPool, artist: &ArtistId, role: Option<&str>) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(DISTINCT member.album_id) FROM track_artist_credits AS credit \
         JOIN album_tracks AS member ON member.track_id = credit.track_id \
         WHERE credit.artist_id = $1 \
           AND (($2::TEXT IS NULL AND credit.role IS NULL) OR credit.role = $2)",
    )
    .bind(artist.as_uuid())
    .bind(role)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn count_equals_the_list_for_every_role(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-role", "Artist Role")
        .await
        .unwrap();

    let alpha = named(&repository, "Alpha").await;
    let composer = named(&repository, "Composer").await;
    let guest = named(&repository, "Guest").await;
    let beta = named(&repository, "Beta").await;

    let alpha_album = repository
        .resolve_or_create_album(&album("Alpha Album").with_album_artist(credit("Alpha", None)))
        .await
        .unwrap();
    let alpha_track = repository
        .get_or_create(
            source("local", "role-alpha.flac"),
            primary("alpha", &["Alpha"]).with_credits(vec![
                credit("Composer", Some(CreditRole::Composer)),
                credit("Guest", Some(CreditRole::Performer)),
            ]),
        )
        .await
        .unwrap();
    place(&repository, alpha_album.id(), alpha_track.id()).await;

    let beta_album = repository
        .resolve_or_create_album(&album("Beta Album").with_album_artist(credit("Beta", None)))
        .await
        .unwrap();
    let beta_track = repository
        .get_or_create(
            source("local", "role-beta.flac"),
            primary("beta", &["Beta"]),
        )
        .await
        .unwrap();
    place(&repository, beta_album.id(), beta_track.id()).await;

    let fixtures: Vec<(&str, Option<RoleSet>, Vec<&ArtistId>)> = vec![
        ("absent", None, vec![&alpha, &composer, &guest, &beta]),
        (
            "albumartist",
            Some(role_set(&[RoleSelector::AlbumArtist])),
            vec![&alpha, &beta],
        ),
        (
            "artist",
            Some(role_set(&[RoleSelector::Artist])),
            vec![&alpha, &beta],
        ),
        (
            "composer",
            Some(role_set(&[RoleSelector::Composer])),
            vec![&composer],
        ),
        (
            "composer and performer",
            Some(role_set(&[RoleSelector::Composer, RoleSelector::Performer])),
            vec![&composer, &guest],
        ),
    ];

    for (name, role, expected) in fixtures {
        let filter = ArtistListFilter::new().with_role(role);
        let observed = listed(&repository, ArtistSort::Name, &filter, Some(&user)).await;
        let observed_total = total(&repository, &filter, Some(&user)).await;

        assert_eq!(
            observed
                .iter()
                .map(|id| *id.as_uuid())
                .collect::<BTreeSet<_>>(),
            artist_set(&expected),
            "the {name} role list"
        );
        assert_eq!(
            observed_total,
            u32::try_from(expected.len()).unwrap(),
            "the {name} role count"
        );
        assert_eq!(
            observed_total,
            u32::try_from(observed.len()).unwrap(),
            "the {name} count is the list length at the full page size"
        );
    }

    let page = listed_at(
        &repository,
        0,
        2,
        ArtistSort::Name,
        &ArtistListFilter::new(),
        Some(&user),
    )
    .await;

    assert_eq!(page.len(), 2);
    assert_eq!(
        total(&repository, &ArtistListFilter::new(), Some(&user)).await,
        4,
        "a truncated page does not shorten the count"
    );
}

#[sqlx::test]
async fn role_scoped_counts_use_the_list_membership(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let x = named(&repository, "X").await;
    let y = named(&repository, "Y").await;

    let composed_album = repository
        .resolve_or_create_album(&album("Composed"))
        .await
        .unwrap();
    let composed_track = repository
        .get_or_create(
            source("local", "composed.flac"),
            primary("composed", &["Other"]).with_credits(vec![
                credit("X", Some(CreditRole::Composer)),
                credit("Y", Some(CreditRole::Composer)),
            ]),
        )
        .await
        .unwrap();
    place(&repository, composed_album.id(), composed_track.id()).await;

    let owned_album = repository
        .resolve_or_create_album(&album("Owned By X").with_album_artist(credit("X", None)))
        .await
        .unwrap();

    for external in ["owned-one.flac", "owned-two.flac"] {
        let track = repository
            .get_or_create(source("local", external), primary(external, &["X"]))
            .await
            .unwrap();
        place(&repository, owned_album.id(), track.id()).await;
    }

    let composers = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::Composer]))),
        None,
    )
    .await;
    let composer_facts = composers
        .iter()
        .map(|(id, albums, songs)| (*id.as_uuid(), (*albums, *songs)))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        composer_facts,
        BTreeSet::from([(*x.as_uuid(), (1, 1)), (*y.as_uuid(), (1, 1)),]),
        "the composer membership reaches album A only"
    );
    assert_eq!(
        count_distinct_albums(&pool, &x, Some("composer")).await,
        1,
        "an independent COUNT(DISTINCT album) agrees"
    );
    assert_eq!(
        count_distinct_tracks(&pool, &x, Some("composer")).await,
        1,
        "an independent COUNT(DISTINCT track) agrees"
    );

    let album_artists = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist]))),
        None,
    )
    .await;

    assert_eq!(
        album_artists,
        vec![(x.clone(), 1, 2)],
        "the album-artist membership reaches album B and its two tracks"
    );
    assert!(
        !album_artists.iter().any(|(id, _, _)| id == &y),
        "the composer credit is not an album-artist membership"
    );
    assert_eq!(
        total(
            &repository,
            &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist]))),
            None,
        )
        .await,
        1
    );

    let absent = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new(),
        None,
    )
    .await;
    let absent_x = absent
        .iter()
        .find(|(id, _, _)| id == &x)
        .expect("X is engaged through its composer credit and its album");
    let absent_y = absent
        .iter()
        .find(|(id, _, _)| id == &y)
        .expect("Y is engaged through its composer credit");

    assert_eq!(
        (absent_x.1, absent_x.2),
        (2, 3),
        "an absent role is the union of every membership"
    );
    assert_eq!(
        (absent_y.1, absent_y.2),
        (1, 1),
        "Y's composer membership reaches album A only"
    );
    assert_eq!(
        count_distinct_albums(&pool, &x, None).await,
        1,
        "X's primary membership alone touches one album"
    );

    let composed_albums = count_distinct_albums(&pool, &x, Some("composer")).await;
    let fronted_albums: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM albums WHERE album_artist_id = $1")
            .bind(x.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!((composed_albums, fronted_albums), (1, 1));
    assert_eq!(
        absent_x.1,
        u32::try_from(composed_albums + fronted_albums).unwrap(),
        "the union adds the two disjoint memberships"
    );
}

#[sqlx::test]
async fn albumartist_role_is_the_album_artist_list(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let fronted = named(&repository, "Fronted").await;
    let other = named(&repository, "Other").await;
    let soloist = named(&repository, "Soloist").await;

    let fronted_album = repository
        .resolve_or_create_album(&album("Fronted Album").with_album_artist(credit("Fronted", None)))
        .await
        .unwrap();
    let fronted_track = repository
        .get_or_create(
            source("local", "fronted.flac"),
            primary("fronted", &["Fronted"]),
        )
        .await
        .unwrap();
    place(&repository, fronted_album.id(), fronted_track.id()).await;

    let other_album = repository
        .resolve_or_create_album(&album("Other Album").with_album_artist(credit("Other", None)))
        .await
        .unwrap();
    let other_track = repository
        .get_or_create(
            source("local", "other.flac"),
            primary("other", &["Soloist"]),
        )
        .await
        .unwrap();
    place(&repository, other_album.id(), other_track.id()).await;

    let album_artists = listed(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist]))),
        None,
    )
    .await;

    assert_eq!(
        album_artists,
        vec![fronted.clone(), other.clone()],
        "exactly the two album artists, in name order"
    );
    assert!(
        !album_artists.contains(&soloist),
        "a credit-only artist is not an album artist"
    );
    assert_eq!(
        total(
            &repository,
            &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist]))),
            None,
        )
        .await,
        2
    );

    let absent = listed(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new(),
        None,
    )
    .await;

    assert_eq!(
        absent
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<BTreeSet<_>>(),
        artist_set(&[&fronted, &other, &soloist]),
        "the whole scoped list adds the credit-only artist"
    );
}

#[sqlx::test]
async fn blank_role_is_absent_and_unknown_role_is_an_error(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (alpha, _, _) = engaged_artist(&repository, "Alpha", "blank-alpha.flac").await;
    let (beta, _, _) = engaged_artist(&repository, "Beta", "blank-beta.flac").await;

    assert_eq!(RoleSelector::parse(""), None);
    assert_eq!(RoleSelector::parse("   "), None);
    assert_eq!(RoleSelector::parse("other"), None);
    assert_eq!(RoleSelector::parse("ALBUMARTIST"), None);
    assert_eq!(RoleSelector::parse("album_artist"), None);
    assert_eq!(RoleSelector::ALL.len(), 13);

    for role in RoleSelector::ALL {
        assert_eq!(RoleSelector::parse(role.as_str()), Some(role));
    }

    assert!(matches!(
        RoleSet::new(std::iter::empty::<RoleSelector>()),
        Err(ValidationError::Empty { field: "ids" })
    ));
    assert_eq!(RoleSelector::Artist.stored_role(), None);
    assert_eq!(RoleSelector::AlbumArtist.stored_role(), None);

    let blank = ArtistListFilter::new().with_role(None);
    let absent = ArtistListFilter::new();

    assert!(blank.role().is_none());
    assert_eq!(
        listed(&repository, ArtistSort::Name, &blank, None).await,
        listed(&repository, ArtistSort::Name, &absent, None).await,
        "a blank role is the same request as no role"
    );
    assert_eq!(
        total(&repository, &blank, None).await,
        total(&repository, &absent, None).await
    );
    assert_eq!(
        listed(&repository, ArtistSort::Name, &blank, None).await,
        vec![alpha.clone(), beta.clone()],
        "a blank role is the whole scoped list, not match-nothing"
    );
}

#[sqlx::test]
async fn counts_dedupe_repeated_memberships(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let wanda = named(&repository, "Wanda").await;

    let album_row = repository
        .resolve_or_create_album(&album("Doubled").with_album_artist(credit("Wanda", None)))
        .await
        .unwrap();
    let doubled = repository
        .get_or_create(
            source("local", "doubled.flac"),
            primary("doubled", &["Wanda", "Wanda"]),
        )
        .await
        .unwrap();
    let single = repository
        .get_or_create(
            source("local", "single.flac"),
            primary("single", &["Wanda"]),
        )
        .await
        .unwrap();
    place(&repository, album_row.id(), doubled.id()).await;
    place(&repository, album_row.id(), single.id()).await;

    let credited_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM track_artist_credits \
         WHERE artist_id = $1 AND role IS NULL",
    )
    .bind(wanda.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        credited_rows, 3,
        "the fixture credits the artist twice on one track"
    );

    let distinct_tracks = count_distinct_tracks(&pool, &wanda, None).await;
    let distinct_albums = count_distinct_albums(&pool, &wanda, None).await;

    assert_eq!((distinct_tracks, distinct_albums), (2, 1));

    let facts = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::Artist]))),
        None,
    )
    .await;

    assert_eq!(
        facts,
        vec![(wanda.clone(), 1, 2)],
        "the primary membership counts the track once and the album once"
    );
    assert_eq!(u32::try_from(distinct_tracks).unwrap(), facts[0].2);
    assert_eq!(u32::try_from(distinct_albums).unwrap(), facts[0].1);

    let album_artist_facts = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::AlbumArtist]))),
        None,
    )
    .await;

    assert_eq!(
        album_artist_facts,
        vec![(wanda.clone(), 1, 2)],
        "the album-artist membership dedupes the same way"
    );

    let listed_once = listed(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_role(Some(role_set(&[RoleSelector::Artist]))),
        None,
    )
    .await;

    assert_eq!(listed_once, vec![wanda]);
}

#[sqlx::test]
async fn play_count_sums_distinct_credited_tracks(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-plays", "Artist Plays")
        .await
        .unwrap();
    let doubled = named(&repository, "Doubled").await;
    let single = named(&repository, "Single").await;

    let doubled_album = repository
        .resolve_or_create_album(&album("Doubled Plays").with_album_artist(credit("Doubled", None)))
        .await
        .unwrap();
    let doubled_track = repository
        .get_or_create(
            source("local", "plays-doubled.flac"),
            primary("plays-doubled", &["Doubled", "Doubled"]),
        )
        .await
        .unwrap();
    place(&repository, doubled_album.id(), doubled_track.id()).await;

    let single_album = repository
        .resolve_or_create_album(&album("Single Plays").with_album_artist(credit("Single", None)))
        .await
        .unwrap();
    let single_track = repository
        .get_or_create(
            source("local", "plays-single.flac"),
            primary("plays-single", &["Single"]),
        )
        .await
        .unwrap();
    place(&repository, single_album.id(), single_track.id()).await;

    for _ in 0..2 {
        repository
            .record_play(user, doubled_track.id(), chrono::Utc::now())
            .await
            .unwrap();
    }
    for _ in 0..3 {
        repository
            .record_play(user, single_track.id(), chrono::Utc::now())
            .await
            .unwrap();
    }

    let stored: i32 = sqlx::query_scalar(
        "SELECT play_count FROM track_play_stats WHERE user_id = $1 AND track_id = $2",
    )
    .bind(user)
    .bind(doubled_track.id().as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(stored, 2, "the fixture plays the doubled track twice");

    let naive: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(plays.play_count), 0) \
         FROM track_artist_credits AS credit \
         JOIN track_play_stats AS plays \
           ON plays.track_id = credit.track_id AND plays.user_id = $1 \
         WHERE credit.artist_id = $2",
    )
    .bind(user)
    .bind(doubled.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        naive, 4,
        "a sum over membership rows would count the doubled credit twice"
    );
    assert!(
        naive > 3,
        "and would outrank the artist with three plays, so the order clause is load bearing"
    );

    assert_eq!(
        listed(
            &repository,
            ArtistSort::PlayCount,
            &ArtistListFilter::new(),
            Some(&user),
        )
        .await,
        vec![single.clone(), doubled.clone()],
        "play_count defaults to descending over distinct credited tracks"
    );
    assert_eq!(
        listed(
            &repository,
            ArtistSort::PlayCount,
            &ArtistListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
            Some(&user),
        )
        .await,
        vec![doubled.clone(), single.clone()],
        "ascending is the reverse value order"
    );
}

#[sqlx::test]
async fn value_order_for_album_count_and_song_count(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut counts = Vec::new();

    for (name, tracks_per_album) in [
        ("Wide", vec![2usize, 1]),
        ("Narrow", vec![1]),
        ("Deep", vec![3]),
    ] {
        let artist = named(&repository, name).await;

        for (index, tracks) in tracks_per_album.into_iter().enumerate() {
            let album_row = repository
                .resolve_or_create_album(
                    &album(&format!("{name} {index}")).with_album_artist(credit(name, None)),
                )
                .await
                .unwrap();

            for member in 0..tracks {
                let external = format!("{name}-{index}-{member}.flac");
                let track = repository
                    .get_or_create(source("local", &external), primary(&external, &[name]))
                    .await
                    .unwrap();
                place(&repository, album_row.id(), track.id()).await;
            }
        }

        counts.push(artist);
    }

    let recomputed_albums = count_distinct_albums(&pool, &counts[0], None).await;
    let recomputed_tracks = count_distinct_tracks(&pool, &counts[0], None).await;

    assert_eq!(
        (recomputed_albums, recomputed_tracks),
        (2, 3),
        "the independent COUNT(DISTINCT ...) recomputation"
    );

    let facts = counted(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new(),
        None,
    )
    .await
    .into_iter()
    .map(|(id, albums, songs)| (id, (albums, songs)))
    .collect::<Vec<_>>();

    assert_eq!(
        facts
            .iter()
            .map(|(id, _)| *id.as_uuid())
            .collect::<BTreeSet<_>>(),
        artist_set(&[&counts[0], &counts[1], &counts[2]])
    );

    let fact_of = |id: &ArtistId| {
        facts
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, fact)| *fact)
            .unwrap()
    };

    assert_eq!(fact_of(&counts[0]), (2, 3));
    assert_eq!(fact_of(&counts[1]), (1, 1));
    assert_eq!(fact_of(&counts[2]), (1, 3));

    let ordered = |values: &[(ArtistId, u32)], reversed: bool| {
        let mut values = values.to_vec();
        values.sort_by(|left, right| {
            let order = right.1.cmp(&left.1);

            if reversed { order.reverse() } else { order }
                .then_with(|| left.0.as_uuid().cmp(right.0.as_uuid()))
        });

        values.into_iter().map(|(id, _)| id).collect::<Vec<_>>()
    };

    let albums = counts
        .iter()
        .cloned()
        .map(|id| {
            let fact = fact_of(&id);
            (id, fact.0)
        })
        .collect::<Vec<_>>();
    let songs = counts
        .iter()
        .cloned()
        .map(|id| {
            let fact = fact_of(&id);
            (id, fact.1)
        })
        .collect::<Vec<_>>();

    for (sort, values, reversed) in [
        (ArtistSort::AlbumCount, &albums, false),
        (ArtistSort::AlbumCount, &albums, true),
        (ArtistSort::SongCount, &songs, false),
        (ArtistSort::SongCount, &songs, true),
    ] {
        let direction = if reversed {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };

        assert_eq!(
            listed(
                &repository,
                sort,
                &ArtistListFilter::new().with_sort_direction(Some(direction)),
                None,
            )
            .await,
            ordered(values, reversed),
            "{sort:?} {direction:?} orders by the count"
        );
    }

    assert_ne!(
        ordered(&albums, false),
        ordered(&songs, false),
        "the two count sorts are different value orders"
    );
}

#[sqlx::test]
async fn name_matches_normalised_text(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let (typographic, _, _) = engaged_artist(&repository, "Placeholder", "named.flac").await;
    let (neighbour, _, _) = engaged_artist(&repository, "Other", "named-plain.flac").await;

    sqlx::query("UPDATE artists SET name = $2 WHERE id = $1")
        .bind(typographic.as_uuid())
        .bind("It\u{2019}s   Just Forever")
        .execute(&pool)
        .await
        .unwrap();

    let matched = listed(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_name(selector("IT'S   JUST forever")),
        None,
    )
    .await;

    assert_eq!(
        matched,
        vec![typographic.clone()],
        "case, whitespace and typographic folding all match the stored normalised name"
    );
    assert_eq!(
        total(
            &repository,
            &ArtistListFilter::new().with_name(selector("IT'S   JUST forever")),
            None,
        )
        .await,
        1
    );

    let partial = listed(
        &repository,
        ArtistSort::Name,
        &ArtistListFilter::new().with_name(selector("just")),
        None,
    )
    .await;

    assert_eq!(partial, vec![typographic.clone()]);

    let blank = ArtistListFilter::new().with_name(selector("   "));

    assert!(blank.name().is_none());
    assert_eq!(
        listed(&repository, ArtistSort::Name, &blank, None).await,
        vec![typographic.clone(), neighbour.clone()],
        "a whitespace-only name is absent, not match-nothing"
    );
    assert_eq!(
        total(&repository, &blank, None).await,
        2,
        "and the count follows the whole scoped list"
    );
}

#[sqlx::test]
async fn absent_values_sort_last_in_both_directions(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-absent", "Artist Absent")
        .await
        .unwrap();

    let (early, _, _) = engaged_artist(&repository, "Early", "absent-early.flac").await;
    let (late, _, _) = engaged_artist(&repository, "Late", "absent-late.flac").await;
    let (plain, _, _) = engaged_artist(&repository, "Plain", "absent-plain.flac").await;

    repository
        .set_starred(user, StateTarget::Artist(&early), true)
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Artist(&late), true)
        .await
        .unwrap();
    age_artist_favorite(&pool, &early, user, -3).await;
    age_artist_favorite(&pool, &late, user, -1).await;
    repository
        .set_rating(user, StateTarget::Artist(&early), Some(3))
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Artist(&late), Some(5))
        .await
        .unwrap();

    for (sort, ascending, descending) in [
        (
            ArtistSort::StarredAt,
            vec![early.clone(), late.clone(), plain.clone()],
            vec![late.clone(), early.clone(), plain.clone()],
        ),
        (
            ArtistSort::Rating,
            vec![early.clone(), late.clone(), plain.clone()],
            vec![late.clone(), early.clone(), plain.clone()],
        ),
    ] {
        assert_eq!(
            listed(
                &repository,
                sort,
                &ArtistListFilter::new().with_sort_direction(Some(SortDirection::Ascending)),
                Some(&user),
            )
            .await,
            ascending,
            "{sort:?} ascending orders by the value and leaves the stateless artist last"
        );
        assert_eq!(
            listed(
                &repository,
                sort,
                &ArtistListFilter::new().with_sort_direction(Some(SortDirection::Descending)),
                Some(&user),
            )
            .await,
            descending,
            "{sort:?} descending reverses the value order and still leaves the stateless artist last"
        );
    }
}

#[sqlx::test]
async fn state_sorts_keep_entities_without_the_state(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-state", "Artist State")
        .await
        .unwrap();

    let (starred, _, _) = engaged_artist(&repository, "Starred", "state-starred.flac").await;
    let (rated, _, _) = engaged_artist(&repository, "Rated", "state-rated.flac").await;
    let (played, _, played_track) =
        engaged_artist(&repository, "Played", "state-played.flac").await;
    let (plain, _, _) = engaged_artist(&repository, "Plain", "state-plain.flac").await;

    repository
        .set_starred(user, StateTarget::Artist(&starred), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Artist(&rated), Some(5))
        .await
        .unwrap();
    repository
        .record_play(user, &played_track, chrono::Utc::now())
        .await
        .unwrap();
    age_artist_favorite(&pool, &starred, user, -1).await;

    let expected = sorted(&[&starred, &rated, &played, &plain]);

    for (sort, first) in [
        (ArtistSort::StarredAt, &starred),
        (ArtistSort::Rating, &rated),
        (ArtistSort::PlayCount, &played),
    ] {
        let observed = listed(&repository, sort, &ArtistListFilter::new(), Some(&user)).await;

        assert_eq!(
            observed.len(),
            4,
            "{sort:?} keeps every engaged artist instead of shortening the list"
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
            "{sort:?} returns the same membership"
        );
        assert_eq!(observed[0], first.clone(), "{sort:?} leads with the state");
        assert_eq!(
            observed[3],
            plain.clone(),
            "{sort:?} leaves the artist without the state last"
        );
        assert_eq!(
            total(&repository, &ArtistListFilter::new(), Some(&user)).await,
            4,
            "the {sort:?} count agrees"
        );
    }
}

#[sqlx::test]
async fn state_sorts_without_a_principal_are_not_empty(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-anonymous", "Artist Anonymous")
        .await
        .unwrap();

    let (one, _, _) = engaged_artist(&repository, "One", "anonymous-one.flac").await;
    let (two, _, _) = engaged_artist(&repository, "Two", "anonymous-two.flac").await;
    let (three, _, _) = engaged_artist(&repository, "Three", "anonymous-three.flac").await;

    repository
        .set_rating(user, StateTarget::Artist(&one), Some(5))
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Artist(&two), true)
        .await
        .unwrap();

    for sort in [
        ArtistSort::Rating,
        ArtistSort::StarredAt,
        ArtistSort::PlayCount,
    ] {
        let observed = listed(&repository, sort, &ArtistListFilter::new(), None).await;

        assert_eq!(
            observed,
            sorted(&[&one, &two, &three]),
            "{sort:?} without a principal is the scoped list ordered by ar.id, not an empty page"
        );
        assert_eq!(
            total(&repository, &ArtistListFilter::new(), None).await,
            u32::try_from(observed.len()).unwrap(),
            "and the count agrees for {sort:?}"
        );
    }
}

#[sqlx::test]
async fn canonical_id_tie_break_is_load_bearing(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut tied = Vec::new();

    for index in 0..3u32 {
        let twin = ArtistCredit::new("Twin".to_owned(), Some(format!("mbid-{index}"))).unwrap();
        let artist = repository.resolve_or_create_artist(&twin).await.unwrap();
        let album_row = repository
            .resolve_or_create_album(&album("Twin Album").with_album_artist(twin.clone()))
            .await
            .unwrap();
        let track = repository
            .get_or_create(
                source("local", &format!("twin-{index}.flac")),
                TrackMetadata::new(format!("twin {index}"), vec![twin], Some(1_000)).unwrap(),
            )
            .await
            .unwrap();
        place(&repository, album_row.id(), track.id()).await;
        tied.push(artist.id().clone());
    }

    let expected = sorted(&[&tied[0], &tied[1], &tied[2]]);

    assert_eq!(
        listed(
            &repository,
            ArtistSort::Name,
            &ArtistListFilter::new(),
            None
        )
        .await,
        expected,
        "equal normalised names order by ar.id"
    );
    assert_eq!(
        listed(
            &repository,
            ArtistSort::Name,
            &ArtistListFilter::new().with_sort_direction(Some(SortDirection::Descending)),
            None,
        )
        .await,
        expected,
        "the canonical tie-break is ar.id ascending in both directions"
    );
    assert_eq!(
        listed(
            &repository,
            ArtistSort::AlbumCount,
            &ArtistListFilter::new(),
            None
        )
        .await,
        expected,
        "equal album counts fall back to the same canonical order"
    );
    assert_eq!(
        counted(
            &repository,
            ArtistSort::AlbumCount,
            &ArtistListFilter::new(),
            None
        )
        .await
        .iter()
        .map(|(_, albums, songs)| (*albums, *songs))
        .collect::<Vec<_>>(),
        vec![(1, 1), (1, 1), (1, 1)],
        "the fixture really ties on album count"
    );

    for (offset, expected) in expected.iter().enumerate() {
        assert_eq!(
            listed_at(
                &repository,
                u32::try_from(offset).unwrap(),
                1,
                ArtistSort::Name,
                &ArtistListFilter::new(),
                None,
            )
            .await,
            vec![expected.clone()],
            "page {offset} of the canonical order"
        );
        assert_eq!(
            listed_at(
                &repository,
                u32::try_from(offset).unwrap(),
                1,
                ArtistSort::AlbumCount,
                &ArtistListFilter::new(),
                None,
            )
            .await,
            vec![expected.clone()],
            "the same page under album_count"
        );
    }

    assert_eq!(
        listed_at(
            &repository,
            3,
            1,
            ArtistSort::Name,
            &ArtistListFilter::new(),
            None,
        )
        .await,
        Vec::new(),
        "the canonical order ends after the third artist"
    );
}

#[test]
fn unknown_sort_is_an_explicit_error() {
    assert_eq!(ArtistSort::ALL.len(), 6);

    for sort in ArtistSort::ALL {
        assert_eq!(ArtistSort::parse(sort.as_str()), Some(sort));
    }

    for value in [
        "random",
        "updatedAt",
        "albumCount",
        "songCount",
        "playCount",
        "starredAt",
        "album_artist",
        "Name",
        "NAME",
        " name",
        "name ",
        "",
    ] {
        assert_eq!(ArtistSort::parse(value), None, "{value}");
    }

    assert_eq!(ArtistSort::default(), ArtistSort::Name);

    let non_default = ArtistSort::ALL
        .into_iter()
        .filter(|sort| *sort != ArtistSort::Name)
        .collect::<Vec<_>>();

    assert_eq!(
        non_default.len(),
        5,
        "every sort but name has a different native value, so no value falls back to name"
    );

    for sort in non_default {
        assert_ne!(sort.as_str(), ArtistSort::Name.as_str());
        assert_eq!(sort.default_direction(), SortDirection::Descending);
    }

    assert_eq!(
        ArtistSort::Name.default_direction(),
        SortDirection::Ascending
    );
}

#[sqlx::test]
async fn artist_selectors_are_scoped(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = provision(pool.clone())
        .create_user("artist-scoped", "Artist Scoped")
        .await
        .unwrap();

    let missing_source = source_in("local", LOCAL_ROOT, "missing.flac");
    let provider_source = source("youtube_music", "vid-scoped");
    let unwalked_source = source_in("local", UNWALKED_ROOT, "unwalked.flac");
    let elsewhere_source = source_in("local", OTHER_ROOT, "elsewhere.flac");
    let source_only_provider = source_in("local", LOCAL_ROOT, "artist-source.flac");

    let mut scoped: Vec<ArtistId> = Vec::new();

    for (name, source) in [
        ("Missing", missing_source.clone()),
        ("Provider", provider_source),
        ("Unwalked", unwalked_source.clone()),
        ("Elsewhere", elsewhere_source.clone()),
    ] {
        let artist = named(&repository, name).await;
        repository
            .get_or_create(source, primary(name, &[name]))
            .await
            .unwrap();
        repository
            .set_starred(user, StateTarget::Artist(&artist), true)
            .await
            .unwrap();
        scoped.push(artist);
    }

    let source_only = named(&repository, "Source Only").await;
    repository
        .set_starred(user, StateTarget::Artist(&source_only), true)
        .await
        .unwrap();
    repository
        .set_artist_source(&source_only, &source_only_provider)
        .await
        .unwrap();

    set_missing(&pool, &missing_source, true).await;

    let missing_filter = ArtistListFilter::new().with_missing(Some(true));
    let missing_list = listed(&repository, ArtistSort::Name, &missing_filter, Some(&user)).await;

    assert_eq!(
        missing_list,
        vec![scoped[0].clone()],
        "only the artist whose only local source is marked missing"
    );
    assert_eq!(total(&repository, &missing_filter, Some(&user)).await, 1);

    let present_filter = ArtistListFilter::new().with_missing(Some(false));
    let present_list = listed(&repository, ArtistSort::Name, &present_filter, Some(&user)).await;

    assert_eq!(
        present_list
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<BTreeSet<_>>(),
        artist_set(&[&scoped[1], &scoped[2], &scoped[3], &source_only]),
        "a provider-only artist and an unwalked root are both not missing"
    );
    assert_eq!(total(&repository, &present_filter, Some(&user)).await, 4);

    let music = register_library(&pool, "scoped main", LOCAL_ROOT).await;
    let elsewhere = register_library(&pool, "scoped elsewhere", OTHER_ROOT).await;

    let main_filter = ArtistListFilter::new().with_library_id(Some(IdSet::new([music]).unwrap()));
    let main_list = listed(&repository, ArtistSort::Name, &main_filter, Some(&user)).await;

    assert_eq!(
        main_list
            .iter()
            .map(|id| *id.as_uuid())
            .collect::<BTreeSet<_>>(),
        artist_set(&[&scoped[0], &source_only]),
        "the /music library holds the credited track and the artist source, and nothing else"
    );
    assert_eq!(total(&repository, &main_filter, Some(&user)).await, 2);

    let elsewhere_filter =
        ArtistListFilter::new().with_library_id(Some(IdSet::new([elsewhere]).unwrap()));
    let elsewhere_list = listed(
        &repository,
        ArtistSort::Name,
        &elsewhere_filter,
        Some(&user),
    )
    .await;

    assert_eq!(
        elsewhere_list,
        vec![scoped[3].clone()],
        "an artist with no source in the requested library is excluded"
    );
    assert_eq!(total(&repository, &elsewhere_filter, Some(&user)).await, 1);

    let unknown = ArtistListFilter::new().with_library_id(Some(
        IdSet::new([LibraryId::from_uuid(Uuid::now_v7())]).unwrap(),
    ));

    assert!(
        repository
            .list_artists(0, 500, ArtistSort::Name, &unknown, Some(user))
            .await
            .is_err(),
        "an unregistered library id is rejected, not ignored"
    );
    assert!(
        repository
            .count_artists(Some(user), &unknown)
            .await
            .is_err(),
        "and the count rejects it too"
    );
}
