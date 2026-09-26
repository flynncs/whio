use std::sync::Arc;

use super::PostgresTrackRepository;
use crate::features::music_library::{
    domain::{
        AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistCredit, ArtworkRef,
        CreditRole, ProviderId, SourceIdentity, SourceScope, StateTarget, TrackId, TrackListFilter,
        TrackListScope, TrackMetadata, TrackOrder, TrackPage,
    },
    storage::contracts::{
        CatalogueWrite, LibraryRead, PlaylistStore, ReconcileReport, SourceFileFacts, SourceLookup,
        StateStore,
    },
};
use uuid::Uuid;

fn source(provider: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider.to_owned()).unwrap(),
        if provider == "local" {
            SourceScope::Named("/music".to_owned())
        } else {
            SourceScope::Global
        },
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

fn identified(title: &str, recording_id: &str) -> TrackMetadata {
    metadata(title)
        .with_recording_id(Some(recording_id.to_owned()))
        .unwrap()
}

fn with_isrc(title: &str, isrc: &str) -> TrackMetadata {
    metadata(title).with_isrc(Some(isrc.to_owned())).unwrap()
}

fn credit(name: &str, identifier: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), identifier.map(str::to_owned)).unwrap()
}

fn album_metadata(title: &str) -> AlbumMetadata {
    AlbumMetadata::new(title.to_owned()).unwrap()
}

#[sqlx::test]
async fn repository_maintains_stable_bidirectional_mappings(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first_source = source("youtube_music", "source-a");
    let second_source = source("youtube_music", "source-b");

    let first_track = repository
        .get_or_create(first_source.clone(), metadata("First"))
        .await
        .unwrap();
    let repeated_track = repository
        .get_or_create(first_source.clone(), metadata("Updated"))
        .await
        .unwrap();
    let second_track = repository
        .get_or_create(second_source, metadata("Second"))
        .await
        .unwrap();

    assert_eq!(first_track.id(), repeated_track.id());
    assert_ne!(first_track.id(), second_track.id());
    assert_eq!(repeated_track.title(), "Updated");
    assert_eq!(
        repository.find_sources(first_track.id()).await.unwrap(),
        vec![first_source]
    );
}

#[sqlx::test]
async fn an_isrc_matches_only_when_it_is_unambiguous(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let first = repository
        .get_or_create(source("local", "a.flac"), with_isrc("Song", "ISRC-1"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(source("local", "b.flac"), with_isrc("Song", "ISRC-1"))
        .await
        .unwrap();
    assert_eq!(first.id(), second.id());

    let ambiguous = repository
        .get_or_create(source("local", "c.flac"), with_isrc("Song", "ISRC-1"))
        .await
        .unwrap();
    assert_eq!(ambiguous.id(), first.id());
}

#[sqlx::test]
async fn a_recording_id_outranks_an_isrc(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let by_isrc = repository
        .get_or_create(source("local", "a.flac"), with_isrc("Song", "ISRC-1"))
        .await
        .unwrap();
    let by_recording = repository
        .get_or_create(source("local", "b.flac"), identified("Song", "rec-1"))
        .await
        .unwrap();
    assert_ne!(by_isrc.id(), by_recording.id());

    let both = metadata("Song")
        .with_recording_id(Some("rec-1".to_owned()))
        .unwrap()
        .with_isrc(Some("ISRC-1".to_owned()))
        .unwrap();
    let resolved = repository
        .get_or_create(source("local", "c.flac"), both)
        .await
        .unwrap();

    assert_eq!(resolved.id(), by_recording.id());
}

#[sqlx::test]
async fn repository_returns_a_stored_track_by_id(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let stored = repository
        .get_or_create(source("youtube_music", "source-a"), metadata("First"))
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
async fn repository_returns_no_sources_for_an_unknown_track(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    assert!(
        repository
            .find_sources(&TrackId::new())
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn artwork_resolves_from_the_source_that_stored_it(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let source = source("youtube_music", "vid-1");

    let track = repository
        .get_or_create(source.clone(), metadata("Song"))
        .await
        .unwrap();

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
async fn artist_ladder_prefers_identifiers_then_normalised_names(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let first = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    let again = repository
        .resolve_or_create_artist(&credit("  daft   punk ", None))
        .await
        .unwrap();
    assert_eq!(first.id(), again.id(), "normalised names match");
    assert_eq!(again.musicbrainz_artist_id(), None);

    let by_name = repository
        .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
        .await
        .unwrap();
    assert_eq!(by_name.id(), first.id(), "an id attaches to the name row");
    assert_eq!(by_name.musicbrainz_artist_id(), Some("mb-1"));

    let by_id = repository
        .resolve_or_create_artist(&credit("Someone Else", Some("mb-1")))
        .await
        .unwrap();
    assert_eq!(by_id.id(), first.id(), "the id wins over the name");
}

#[sqlx::test]
async fn an_id_bearing_and_a_name_only_artist_coexist(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    let named = repository
        .resolve_or_create_artist(&credit("Daft Punk", Some("mb-1")))
        .await
        .unwrap();
    let plain = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();

    assert_ne!(named.id(), plain.id());
    assert_eq!(named.musicbrainz_artist_id(), Some("mb-1"));
    assert_eq!(plain.musicbrainz_artist_id(), None);
}

#[sqlx::test]
async fn album_ladder_matches_identifiers_then_normalised_text(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = credit("Daft Punk", None);

    let first = repository
        .resolve_or_create_album(&album_metadata("Discovery").with_album_artist(artist.clone()))
        .await
        .unwrap();
    let same_text = repository
        .resolve_or_create_album(&album_metadata("  discovery ").with_album_artist(artist))
        .await
        .unwrap();
    assert_eq!(first.id(), same_text.id());

    let by_group = repository
        .resolve_or_create_album(
            &album_metadata("Totally Different")
                .with_release_group_id(Some("rg-1".to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let same_group = repository
        .resolve_or_create_album(
            &album_metadata("Also Different")
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
async fn re_filing_a_track_replaces_its_membership(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .resolve_or_create_album(&album_metadata("First"))
        .await
        .unwrap();
    let second = repository
        .resolve_or_create_album(&album_metadata("Second"))
        .await
        .unwrap();
    let track = repository
        .get_or_create(source("local", "path-a"), metadata("Song"))
        .await
        .unwrap();
    let membership = AlbumTrack::new(Some(1), Some(1), 0).unwrap();

    repository
        .set_album_track(first.id(), track.id(), &membership)
        .await
        .unwrap();
    repository
        .set_album_track(second.id(), track.id(), &membership)
        .await
        .unwrap();

    assert_eq!(
        repository
            .find_album_for_track(track.id())
            .await
            .unwrap()
            .expect("the track is filed once")
            .id(),
        second.id()
    );
    assert!(
        repository
            .find_album_entries(first.id(), None)
            .await
            .unwrap()
            .is_empty(),
        "the previous album no longer holds the track"
    );
    assert_eq!(
        repository
            .find_album_entries(second.id(), None)
            .await
            .unwrap()
            .len(),
        1
    );

    repository.clear_album_track(track.id()).await.unwrap();
    assert!(
        repository
            .find_album_for_track(track.id())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repository
            .find_album_entries(second.id(), None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn album_entries_are_ordered_and_totalled(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let artist = repository
        .resolve_or_create_artist(&credit("Daft Punk", None))
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(
            &album_metadata("Discovery").with_album_artist(credit("Daft Punk", None)),
        )
        .await
        .unwrap();
    let album_track = |title: &str| {
        TrackMetadata::from_names(title.to_owned(), vec!["Daft Punk".to_owned()], Some(1_000))
            .unwrap()
    };

    let third = repository
        .get_or_create(source("local", "path-c"), album_track("Third"))
        .await
        .unwrap();
    let first = repository
        .get_or_create(source("local", "path-a"), album_track("First"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(source("local", "path-b"), album_track("Second"))
        .await
        .unwrap();

    for (track, membership) in [
        (&third, AlbumTrack::new(Some(1), Some(3), 2).unwrap()),
        (&first, AlbumTrack::new(Some(1), Some(1), 0).unwrap()),
        (&second, AlbumTrack::new(None, None, 0).unwrap()),
    ] {
        repository
            .set_album_track(album.id(), track.id(), &membership)
            .await
            .unwrap();
    }

    let titles = repository
        .find_album_entries(album.id(), None)
        .await
        .unwrap()
        .iter()
        .map(|entry| entry.track().title().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        ["First", "Third", "Second"],
        "numbered tracks come first in track order, unnumbered last"
    );

    let summaries = repository
        .list_albums(0, 10, AlbumOrder::Title, AlbumListFilter::default(), None)
        .await
        .unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].album().id(), album.id());
    assert_eq!(summaries[0].album_artist().unwrap().id(), artist.id());
    assert_eq!(summaries[0].track_count(), 3);
    assert_eq!(summaries[0].duration_ms(), 3_000);

    let albums = repository
        .find_albums_for_artist(artist.id(), None)
        .await
        .unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].album().id(), album.id());

    let artists = repository.list_artists(0, 10, None).await.unwrap();
    let album_artist = artists
        .iter()
        .find(|item| item.artist().id() == artist.id())
        .unwrap();
    assert_eq!(album_artist.album_count(), 1);
}

#[sqlx::test]
async fn search_matches_titles_and_artists_and_ranks_owned_first(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let owned = repository
        .get_or_create(
            source("local", "path-a"),
            TrackMetadata::from_names(
                "Zebra".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let remote = repository
        .get_or_create(
            source("youtube_music", "vid-1"),
            TrackMetadata::from_names(
                "Apple".to_owned(),
                vec!["Daft Punk".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();

    let by_artist = repository
        .search_tracks("daft punk", 10, None)
        .await
        .unwrap();
    assert_eq!(
        by_artist
            .iter()
            .map(|hit| hit.track().id().clone())
            .collect::<Vec<_>>(),
        vec![owned.id().clone(), remote.id().clone()],
        "the owned track outranks the remote one despite the title"
    );

    let by_title = repository.search_tracks("zeb", 10, None).await.unwrap();
    assert_eq!(by_title.len(), 1);
    assert_eq!(by_title[0].track().id(), owned.id());
    assert_eq!(by_title[0].sources().len(), 1);

    assert!(
        repository
            .search_tracks("nothing here", 10, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn search_uses_ownership_inside_a_tier(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let remote = repository
        .get_or_create(
            source("youtube_music", "vid-exact"),
            TrackMetadata::from_names("Zebra".to_owned(), vec!["Other".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap();
    let owned = repository
        .get_or_create(
            source("local", "path-exact"),
            TrackMetadata::from_names("Zebra".to_owned(), vec!["Other".to_owned()], Some(1_000))
                .unwrap(),
        )
        .await
        .unwrap();

    let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].track().id(), owned.id());
    assert_eq!(hits[1].track().id(), remote.id());
}

#[sqlx::test]
async fn candidate_tracks_batches_normalized_title_lookups(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .get_or_create(
            source("local", "path-a"),
            TrackMetadata::from_names(
                "It\u{2019}s Just Forever".to_owned(),
                vec!["Cage the Elephant".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .get_or_create(
            source("local", "path-b"),
            TrackMetadata::from_names("Other".to_owned(), vec!["Other".to_owned()], Some(1_000))
                .unwrap(),
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

#[sqlx::test]
async fn search_matches_albums_and_their_artists(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    repository
        .resolve_or_create_album(
            &album_metadata("Discovery").with_album_artist(credit("Daft Punk", None)),
        )
        .await
        .unwrap();

    assert_eq!(
        repository
            .search_albums("disc", 10, None)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repository
            .search_albums("daft", 10, None)
            .await
            .unwrap()
            .len(),
        1,
        "the album artist name matches the album"
    );
    assert!(
        repository
            .search_albums("nope", 10, None)
            .await
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        repository
            .search_artists("daft", 10, None)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        repository
            .search_artists("nope", 10, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn albums_order_by_title_artist_and_creation(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let first = repository
        .resolve_or_create_album(&album_metadata("Zebra").with_album_artist(credit("A Band", None)))
        .await
        .unwrap();
    let second = repository
        .resolve_or_create_album(&album_metadata("Apple").with_album_artist(credit("Z Band", None)))
        .await
        .unwrap();
    for (album, name) in [(&first, "Zebra"), (&second, "Apple")] {
        let track = repository
            .get_or_create(source("local", &format!("{name}.flac")), metadata(name))
            .await
            .unwrap();
        repository
            .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }

    let by_title = repository
        .list_albums(0, 10, AlbumOrder::Title, AlbumListFilter::default(), None)
        .await
        .unwrap();
    assert_eq!(by_title[0].album().id(), second.id());
    assert_eq!(by_title[1].album().id(), first.id());

    let by_artist = repository
        .list_albums(0, 10, AlbumOrder::Artist, AlbumListFilter::default(), None)
        .await
        .unwrap();
    assert_eq!(by_artist[0].album().id(), first.id());

    let recent = repository
        .list_albums(
            0,
            10,
            AlbumOrder::RecentlyAdded,
            AlbumListFilter::default(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(recent[0].album().id(), second.id());
}

#[sqlx::test]
async fn per_user_state_round_trips_for_every_entity(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("contract-user", "Contract User")
        .await
        .unwrap();
    let track = repository
        .get_or_create(source("local", "path-a"), metadata("Song"))
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&album_metadata("Discovery"))
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
        .set_rating(user, StateTarget::Track(track.id()), Some(4))
        .await
        .unwrap();
    repository
        .record_play(user, track.id(), chrono::Utc::now())
        .await
        .unwrap();
    repository
        .record_play(user, track.id(), chrono::Utc::now())
        .await
        .unwrap();

    let state = repository.find_track_state(track.id(), user).await.unwrap();
    assert!(state.is_starred());
    assert_eq!(state.rating(), Some(4));
    assert_eq!(state.play_count(), 2);
    assert!(
        repository
            .find_track_state(track.id(), Uuid::now_v7())
            .await
            .unwrap()
            .play_count()
            == 0,
        "state is per user"
    );

    repository
        .set_starred(user, StateTarget::Album(album.id()), true)
        .await
        .unwrap();
    let summary = repository
        .find_album_summary(album.id(), Some(user))
        .await
        .unwrap()
        .unwrap();
    assert!(summary.state().is_starred());

    repository
        .set_starred(user, StateTarget::Artist(artist.id()), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Artist(artist.id()), Some(5))
        .await
        .unwrap();
    let artist_state = repository
        .find_artist_state(artist.id(), user)
        .await
        .unwrap();
    assert!(artist_state.is_starred());
    assert_eq!(artist_state.rating(), Some(5));

    let starred = repository.starred(user).await.unwrap();
    assert_eq!(starred.tracks().len(), 1);
    assert_eq!(starred.albums().len(), 1);
    assert_eq!(starred.artists().len(), 1);

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
async fn album_orders_reflect_plays_and_ratings(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("contract-user", "Contract User")
        .await
        .unwrap();
    let first = repository
        .resolve_or_create_album(&album_metadata("First"))
        .await
        .unwrap();
    let second = repository
        .resolve_or_create_album(&album_metadata("Second"))
        .await
        .unwrap();
    let first_track = repository
        .get_or_create(source("local", "path-a"), metadata("A"))
        .await
        .unwrap();
    let second_track = repository
        .get_or_create(source("local", "path-b"), metadata("B"))
        .await
        .unwrap();
    repository
        .set_album_track(first.id(), first_track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(second.id(), second_track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    repository
        .record_play(user, second_track.id(), chrono::Utc::now())
        .await
        .unwrap();
    repository
        .record_play(user, second_track.id(), chrono::Utc::now())
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Album(first.id()), Some(5))
        .await
        .unwrap();

    let frequent = repository
        .list_albums(
            0,
            10,
            AlbumOrder::Frequent,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(frequent[0].album().id(), second.id());
    assert_eq!(frequent[0].state().play_count(), 2);

    let highest = repository
        .list_albums(
            0,
            10,
            AlbumOrder::HighestRated,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(highest[0].album().id(), first.id());

    let recent = repository
        .list_albums(
            0,
            10,
            AlbumOrder::Recent,
            AlbumListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(recent[0].album().id(), second.id());
}

#[sqlx::test]
async fn genres_aggregate_and_filter_albums(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&album_metadata("Discovery").with_year(Some(2001)).unwrap())
        .await
        .unwrap();
    let rock = repository
        .get_or_create(
            source("local", "path-a"),
            metadata("One").with_genres(vec!["Rock".to_owned()]),
        )
        .await
        .unwrap();
    let pop = repository
        .get_or_create(
            source("local", "path-b"),
            metadata("Two").with_genres(vec!["Pop".to_owned()]),
        )
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), rock.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), pop.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let genres = repository.list_genres().await.unwrap();
    assert_eq!(genres.len(), 2);
    assert!(
        genres
            .iter()
            .any(|genre| genre.genre() == "Rock" && genre.song_count() == 1)
    );

    let rock_albums = repository
        .list_albums(
            0,
            10,
            AlbumOrder::Title,
            AlbumListFilter::new().with_genre(Some("rock".to_owned())),
            None,
        )
        .await
        .unwrap();
    assert_eq!(rock_albums.len(), 1);
    assert_eq!(rock_albums[0].album().id(), album.id());

    let by_year = repository
        .list_albums(
            0,
            10,
            AlbumOrder::YearAscending,
            AlbumListFilter::new().with_year(Some(2000), Some(2005)),
            None,
        )
        .await
        .unwrap();
    assert_eq!(by_year.len(), 1);

    let none = repository
        .list_albums(
            0,
            10,
            AlbumOrder::YearAscending,
            AlbumListFilter::new().with_year(Some(2010), Some(2015)),
            None,
        )
        .await
        .unwrap();
    assert!(none.is_empty());
}

fn ids_of(page: &TrackPage) -> Vec<TrackId> {
    page.items()
        .iter()
        .map(|item| item.track().id().clone())
        .collect()
}

fn sorted_ids(ids: &[TrackId]) -> Vec<TrackId> {
    let mut sorted = ids.to_vec();
    sorted.sort_by(|left, right| left.as_uuid().cmp(right.as_uuid()));
    sorted
}

async fn set_created_at(
    pool: &sqlx::PgPool,
    track: &TrackId,
    created_at: chrono::DateTime<chrono::Utc>,
) {
    sqlx::query("UPDATE tracks SET created_at = $1 WHERE id = $2")
        .bind(created_at)
        .bind(track.as_uuid())
        .execute(pool)
        .await
        .unwrap();
}

#[sqlx::test]
async fn page_tracks_returns_matching_total_and_stable_order(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let mut fixture = Vec::new();

    for index in 0..1001u32 {
        let title = format!("item{:03}", index % 10);
        let track = repository
            .get_or_create(
                source("local", &format!("path-{index:04}")),
                metadata(&title),
            )
            .await
            .unwrap();

        fixture.push((title, track.id().clone()));
    }

    fixture.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_uuid().cmp(right.1.as_uuid()))
    });
    let expected = fixture.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
    let filter = TrackListFilter::new();
    let mut totals = Vec::new();

    for offset in [0u32, 500, 1000] {
        let page = repository
            .page_tracks(offset, 500, TrackOrder::Title, &filter, None)
            .await
            .unwrap();
        let expected_page = expected
            .iter()
            .skip(offset as usize)
            .take(500)
            .cloned()
            .collect::<Vec<_>>();

        assert_eq!(page.total(), 1001, "the total at offset {offset}");
        assert_eq!(ids_of(&page), expected_page, "the page at offset {offset}");
        assert_eq!(
            page.items().len(),
            500usize.min(1001 - offset as usize),
            "the page size at offset {offset}"
        );
        totals.push(page.total());
    }

    assert_eq!(totals, [1001, 1001, 1001]);
}

#[sqlx::test]
async fn deep_offset_beyond_end_reports_exact_total(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("page-user", "Page User")
        .await
        .unwrap();

    for index in 0..3 {
        repository
            .get_or_create(
                source("local", &format!("owned-{index}")),
                metadata(&format!("owned{index}")),
            )
            .await
            .unwrap();
    }
    for index in 0..2 {
        let track = repository
            .get_or_create(
                source("youtube_music", &format!("vid-{index}")),
                metadata(&format!("remote{index}")),
            )
            .await
            .unwrap();
        repository
            .set_starred(user, StateTarget::Track(track.id()), true)
            .await
            .unwrap();
    }

    let engaged = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(engaged.items().len(), 5);

    let engaged_past_end = repository
        .page_tracks(
            15,
            500,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert!(engaged_past_end.items().is_empty());
    assert_eq!(engaged_past_end.total(), 5);

    let owned = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new().with_scope(TrackListScope::Owned),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(owned.items().len(), 3);

    let owned_past_end = repository
        .page_tracks(
            13,
            500,
            TrackOrder::Title,
            &TrackListFilter::new().with_scope(TrackListScope::Owned),
            Some(user),
        )
        .await
        .unwrap();
    assert!(owned_past_end.items().is_empty());
    assert_eq!(owned_past_end.total(), 3);
}

#[sqlx::test]
async fn engaged_scope_is_principal_derived(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let provision = crate::features::identity::provision(pool.clone());
    let listener = provision.create_user("listener", "Listener").await.unwrap();
    let stranger = provision.create_user("stranger", "Stranger").await.unwrap();

    let owned = repository
        .get_or_create(source("local", "path-owned"), metadata("owned"))
        .await
        .unwrap();
    let favourited = repository
        .get_or_create(source("youtube_music", "vid-fav"), metadata("favourited"))
        .await
        .unwrap();
    let rated = repository
        .get_or_create(source("youtube_music", "vid-rated"), metadata("rated"))
        .await
        .unwrap();
    let played = repository
        .get_or_create(source("youtube_music", "vid-played"), metadata("played"))
        .await
        .unwrap();
    let listed = repository
        .get_or_create(source("youtube_music", "vid-listed"), metadata("listed"))
        .await
        .unwrap();

    repository
        .set_starred(listener, StateTarget::Track(favourited.id()), true)
        .await
        .unwrap();
    repository
        .set_rating(listener, StateTarget::Track(rated.id()), Some(4))
        .await
        .unwrap();
    repository
        .record_play(listener, played.id(), chrono::Utc::now())
        .await
        .unwrap();
    let playlist = repository
        .create_playlist(listener, "Mixed", None)
        .await
        .unwrap();
    repository
        .add_playlist_track(listener, playlist.id(), listed.id())
        .await
        .unwrap();

    let for_listener = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(listener),
        )
        .await
        .unwrap();
    let expected = sorted_ids(&[
        owned.id().clone(),
        favourited.id().clone(),
        rated.id().clone(),
        played.id().clone(),
        listed.id().clone(),
    ]);
    assert_eq!(sorted_ids(&ids_of(&for_listener)), expected);
    assert_eq!(for_listener.total(), 5);

    let for_stranger = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(stranger),
        )
        .await
        .unwrap();
    assert_eq!(ids_of(&for_stranger), vec![owned.id().clone()]);
    assert_eq!(for_stranger.total(), 1);
}

#[sqlx::test]
async fn anonymous_engaged_scope_equals_owned(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("anonymous-user", "Anonymous User")
        .await
        .unwrap();

    let mut owned_ids = Vec::new();
    for index in 0..2 {
        let track = repository
            .get_or_create(
                source("local", &format!("owned-{index}")),
                metadata(&format!("owned{index}")),
            )
            .await
            .unwrap();

        owned_ids.push(track.id().clone());
    }
    let favourited = repository
        .get_or_create(source("youtube_music", "vid-fav"), metadata("favourited"))
        .await
        .unwrap();
    let played = repository
        .get_or_create(source("youtube_music", "vid-played"), metadata("played"))
        .await
        .unwrap();
    repository
        .set_starred(user, StateTarget::Track(favourited.id()), true)
        .await
        .unwrap();
    repository
        .record_play(user, played.id(), chrono::Utc::now())
        .await
        .unwrap();

    let anonymous = repository
        .page_tracks(0, 500, TrackOrder::Title, &TrackListFilter::new(), None)
        .await
        .unwrap();
    let owned = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new().with_scope(TrackListScope::Owned),
            None,
        )
        .await
        .unwrap();

    assert_eq!(ids_of(&anonymous), owned_ids);
    assert_eq!(anonymous.total(), 2);
    assert_eq!(ids_of(&anonymous), ids_of(&owned));
    assert_eq!(owned.total(), 2);
}

#[sqlx::test]
async fn owned_scope_excludes_engaged_only_tracks(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("scope-user", "Scope User")
        .await
        .unwrap();

    let first = repository
        .get_or_create(source("local", "path-first"), metadata("first"))
        .await
        .unwrap();
    let second = repository
        .get_or_create(source("local", "path-second"), metadata("second"))
        .await
        .unwrap();
    let favourited = repository
        .get_or_create(source("youtube_music", "vid-fav"), metadata("favourited"))
        .await
        .unwrap();
    let rated = repository
        .get_or_create(source("youtube_music", "vid-rated"), metadata("rated"))
        .await
        .unwrap();
    let played = repository
        .get_or_create(source("youtube_music", "vid-played"), metadata("played"))
        .await
        .unwrap();

    repository
        .set_starred(user, StateTarget::Track(favourited.id()), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(rated.id()), Some(3))
        .await
        .unwrap();
    repository
        .record_play(user, played.id(), chrono::Utc::now())
        .await
        .unwrap();

    let owned = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new().with_scope(TrackListScope::Owned),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(
        sorted_ids(&ids_of(&owned)),
        sorted_ids(&[first.id().clone(), second.id().clone()])
    );
    assert_eq!(owned.total(), 2);

    let engaged = repository
        .page_tracks(
            0,
            500,
            TrackOrder::Title,
            &TrackListFilter::new(),
            Some(user),
        )
        .await
        .unwrap();
    assert_eq!(engaged.total(), 5);
}

#[sqlx::test]
async fn recently_added_orders_descending_with_id_tie_break(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let oldest = repository
        .get_or_create(source("local", "path-oldest"), metadata("oldest"))
        .await
        .unwrap();
    let newer = repository
        .get_or_create(source("local", "path-newer"), metadata("newer"))
        .await
        .unwrap();
    let tied = repository
        .get_or_create(source("local", "path-tied"), metadata("tied"))
        .await
        .unwrap();

    let newest_at = chrono::Utc::now();
    set_created_at(&pool, oldest.id(), newest_at - chrono::Duration::days(2)).await;
    set_created_at(&pool, newer.id(), newest_at).await;
    set_created_at(&pool, tied.id(), newest_at).await;

    let expected = if tied.id().as_uuid() < newer.id().as_uuid() {
        vec![tied.id().clone(), newer.id().clone(), oldest.id().clone()]
    } else {
        vec![newer.id().clone(), tied.id().clone(), oldest.id().clone()]
    };

    let first = repository
        .page_tracks(
            0,
            10,
            TrackOrder::RecentlyAdded,
            &TrackListFilter::new(),
            None,
        )
        .await
        .unwrap();
    let second = repository
        .page_tracks(
            0,
            10,
            TrackOrder::RecentlyAdded,
            &TrackListFilter::new(),
            None,
        )
        .await
        .unwrap();

    assert_eq!(ids_of(&first), expected);
    assert_eq!(ids_of(&second), expected);
    assert_eq!(first.total(), 3);
}

#[sqlx::test]
async fn page_items_carry_album_membership_state_and_sources(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let user = crate::features::identity::provision(pool.clone())
        .create_user("deep-user", "Deep User")
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&album_metadata("Discovery"))
        .await
        .unwrap();

    let alpha = repository
        .get_or_create(source("local", "path-alpha"), metadata("alpha"))
        .await
        .unwrap();
    let beta = repository
        .get_or_create(source("local", "path-beta"), metadata("beta"))
        .await
        .unwrap();
    let gamma = repository
        .get_or_create(
            source("local", "path-gamma"),
            identified("gamma", "rec-gamma"),
        )
        .await
        .unwrap();
    repository
        .get_or_create(
            source("youtube_music", "vid-gamma"),
            identified("gamma", "rec-gamma"),
        )
        .await
        .unwrap();

    for (track, number) in [(&alpha, 1u16), (&beta, 2), (&gamma, 3)] {
        repository
            .set_album_track(
                album.id(),
                track.id(),
                &AlbumTrack::new(Some(2), Some(number), u32::from(number) - 1).unwrap(),
            )
            .await
            .unwrap();
    }

    repository
        .set_starred(user, StateTarget::Track(gamma.id()), true)
        .await
        .unwrap();
    repository
        .set_rating(user, StateTarget::Track(gamma.id()), Some(5))
        .await
        .unwrap();
    repository
        .record_play(user, gamma.id(), chrono::Utc::now())
        .await
        .unwrap();

    let page = repository
        .page_tracks(2, 1, TrackOrder::Title, &TrackListFilter::new(), Some(user))
        .await
        .unwrap();
    assert_eq!(page.total(), 3);
    assert_eq!(page.items().len(), 1);

    let item = &page.items()[0];
    assert_eq!(item.track().id(), gamma.id());
    assert_eq!(item.track().title(), "gamma");
    assert_eq!(item.album().map(|album| album.id()), Some(album.id()));
    assert_eq!(item.album().map(|album| album.title()), Some("Discovery"));
    let membership = item.membership().expect("the item carries its membership");
    assert_eq!(membership.disc_number(), Some(2));
    assert_eq!(membership.track_number(), Some(3));
    assert_eq!(membership.position(), 2);
    assert!(item.state().is_starred());
    assert_eq!(item.state().rating(), Some(5));
    assert_eq!(item.state().play_count(), 1);
    let mut providers = item
        .sources()
        .iter()
        .map(|source| source.provider_id().as_str().to_owned())
        .collect::<Vec<_>>();
    providers.sort();
    assert_eq!(providers, ["local", "youtube_music"]);
}

#[sqlx::test]
async fn the_artist_list_keeps_primary_credits_only(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let credits = vec![
        ArtistCredit::new("Clara Schumann".to_owned(), None)
            .unwrap()
            .with_role(CreditRole::Composer),
        ArtistCredit::new("Bill Evans".to_owned(), None)
            .unwrap()
            .with_role(CreditRole::Performer),
    ];
    let track = repository
        .get_or_create(
            source("local", "path-a"),
            metadata("Song").with_credits(credits),
        )
        .await
        .unwrap();

    let artists = repository.list_artists(0, 10, None).await.unwrap();
    let names = artists
        .iter()
        .map(|artist| artist.artist().name().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Artist"], "a role credit is not a browsable artist");
    assert_eq!(repository.count_artists(None).await.unwrap(), 1);

    let stored = repository
        .find_track(track.id())
        .await
        .unwrap()
        .expect("the track is stored");
    assert_eq!(stored.artists().len(), 1);
    assert_eq!(
        stored
            .metadata()
            .credits()
            .iter()
            .map(|credit| (credit.name(), credit.role()))
            .collect::<Vec<_>>(),
        [
            ("Clara Schumann", Some(CreditRole::Composer)),
            ("Bill Evans", Some(CreditRole::Performer)),
        ],
        "the role credits are still stored and read back"
    );
}

#[sqlx::test]
async fn the_genre_facets_use_the_set(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let album = repository
        .resolve_or_create_album(&album_metadata("Discovery"))
        .await
        .unwrap();
    let rock_and_pop = repository
        .get_or_create(
            source("local", "path-a"),
            metadata("One").with_genres(vec!["Pop".to_owned(), "Rock".to_owned()]),
        )
        .await
        .unwrap();
    let jazz = repository
        .get_or_create(
            source("local", "path-b"),
            metadata("Two").with_genres(vec!["Jazz".to_owned()]),
        )
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), rock_and_pop.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), jazz.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let genres = repository.list_genres().await.unwrap();
    assert_eq!(genres.len(), 3, "each value of a multi-valued track counts");
    for genre in &genres {
        assert_eq!(genre.song_count(), 1, "{}", genre.genre());
        assert_eq!(genre.album_count(), 1, "{}", genre.genre());
    }
    let mut names = genres
        .iter()
        .map(|genre| genre.genre().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["Jazz", "Pop", "Rock"]);

    let filter = AlbumListFilter::new().with_genre(Some("rock".to_owned()));
    let rock_albums = repository
        .list_albums(0, 10, AlbumOrder::Title, filter.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        rock_albums.len(),
        1,
        "a non-first genre still finds the album"
    );
    assert_eq!(rock_albums[0].album().id(), album.id());
    assert_eq!(
        repository.count_albums(None, filter).await.unwrap(),
        1,
        "the count and the page move together"
    );

    let absent = AlbumListFilter::new().with_genre(Some("Polka".to_owned()));
    assert!(
        repository
            .list_albums(0, 10, AlbumOrder::Title, absent.clone(), None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(repository.count_albums(None, absent).await.unwrap(), 0);
}

#[derive(Default)]
struct StatementFields {
    statement: String,
}

impl tracing::field::Visit for StatementFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "db.statement" {
            self.statement = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "db.statement" {
            self.statement = format!("{value:?}");
        }
    }
}

struct StatementLog(Arc<parking_lot::Mutex<Vec<String>>>);

impl tracing::Subscriber for StatementLog {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "sqlx::query"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = StatementFields::default();
        event.record(&mut fields);
        self.0.lock().push(fields.statement);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[sqlx::test]
async fn batch_reads_do_not_add_a_per_track_query(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());

    for index in 0..20 {
        repository
            .get_or_create(
                source("local", &format!("path-{index}")),
                metadata(&format!("Song {index:02}")).with_genres(vec!["Rock".to_owned()]),
            )
            .await
            .unwrap();
    }

    let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let dispatch = tracing::Dispatch::new(StatementLog(Arc::clone(&log)));
    let guard = tracing::dispatcher::set_default(&dispatch);
    let filter = TrackListFilter::new();

    log.lock().clear();
    let small = repository
        .page_tracks(0, 2, TrackOrder::Title, &filter, None)
        .await
        .unwrap();
    let small_statements = log.lock().clone();

    log.lock().clear();
    let large = repository
        .page_tracks(0, 20, TrackOrder::Title, &filter, None)
        .await
        .unwrap();
    let large_statements = log.lock().clone();
    drop(guard);

    assert_eq!(small.items().len(), 2);
    assert_eq!(large.items().len(), 20);
    assert!(
        !small_statements.is_empty(),
        "the counter sees sqlx statements"
    );
    assert_eq!(
        small_statements.len(),
        large_statements.len(),
        "a page of 20 must not run more statements than a page of 2"
    );
    assert_eq!(
        small_statements
            .iter()
            .filter(|statement| statement.contains("track_genres"))
            .count(),
        1,
        "one batch read for the genre set"
    );
    assert_eq!(
        small_statements
            .iter()
            .filter(|statement| statement.contains("track_artist_credits"))
            .count(),
        1,
        "one batch read for the credits"
    );
}
fn local_facts(path: &str, missing_at: Option<chrono::DateTime<chrono::Utc>>) -> SourceFileFacts {
    SourceFileFacts {
        path: Some(path.to_owned()),
        size_bytes: 42,
        modified_at: None,
        missing_at,
        content_type: None,
        suffix: Some("flac".to_owned()),
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

async fn logged_statements(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT statements FROM statement_log")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn local_row(repository: &PostgresTrackRepository, external_id: &str) -> SourceFileFacts {
    repository
        .find_source_file_facts(&source("local", external_id))
        .await
        .unwrap()
        .expect("the local row is recorded")
}

#[sqlx::test]
async fn reconciliation_marks_absent_rows_with_the_supplied_timestamp(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let provider = ProviderId::new("local".to_owned()).unwrap();
    let scope = SourceScope::Named("/music".to_owned());
    let disappeared_at: chrono::DateTime<chrono::Utc> = "2026-08-01T10:00:00Z".parse().unwrap();
    let now: chrono::DateTime<chrono::Utc> = "2026-09-20T10:00:00Z".parse().unwrap();
    let later: chrono::DateTime<chrono::Utc> = "2026-09-21T10:00:00Z".parse().unwrap();

    for (path, missing_at) in [
        ("kept.flac", None),
        ("absent.flac", None),
        ("already.flac", Some(disappeared_at)),
    ] {
        repository
            .index_source(
                source("local", path),
                metadata(path),
                &local_facts(path, missing_at),
            )
            .await
            .unwrap();
    }

    let remote = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "video-1".to_owned(),
    )
    .unwrap();
    repository
        .get_or_create(remote.clone(), metadata("Remote"))
        .await
        .unwrap();

    let report = repository
        .reconcile_missing(&provider, &scope, &["kept.flac".to_owned()], now)
        .await
        .unwrap();

    assert_eq!(
        report,
        ReconcileReport {
            marked_missing: 1,
            reappeared: 0,
        }
    );
    assert_eq!(
        local_row(&repository, "absent.flac").await.missing_at,
        Some(now),
        "the stored instant is the caller's, not the clock's"
    );
    assert_eq!(
        local_row(&repository, "already.flac").await.missing_at,
        Some(disappeared_at)
    );
    assert_eq!(local_row(&repository, "kept.flac").await.missing_at, None);

    let report = repository
        .reconcile_missing(
            &provider,
            &scope,
            &["kept.flac".to_owned(), "absent.flac".to_owned()],
            later,
        )
        .await
        .unwrap();

    assert_eq!(
        report,
        ReconcileReport {
            marked_missing: 0,
            reappeared: 1,
        }
    );
    assert_eq!(
        local_row(&repository, "absent.flac").await.missing_at,
        None,
        "the seen path is cleared"
    );
    assert_eq!(
        local_row(&repository, "already.flac").await.missing_at,
        Some(disappeared_at),
        "a path the walk did not see keeps its disappearance date"
    );

    let remote_row = repository
        .find_source_file_facts(&remote)
        .await
        .unwrap()
        .expect("the provider source is recorded");
    assert_eq!(remote_row.path, None);
    assert_eq!(remote_row.missing_at, None);
}

#[sqlx::test]
async fn reconciliation_statement_count_does_not_follow_the_row_count(pool: sqlx::PgPool) {
    let repository = PostgresTrackRepository::new(pool.clone());
    let provider = ProviderId::new("local".to_owned()).unwrap();
    let dense = SourceScope::Named("/music".to_owned());
    let sparse = SourceScope::Named("/music-sparse".to_owned());
    let now: chrono::DateTime<chrono::Utc> = "2026-09-20T10:00:00Z".parse().unwrap();
    let later: chrono::DateTime<chrono::Utc> = "2026-09-21T10:00:00Z".parse().unwrap();
    let dense_paths = (0..40)
        .map(|index| format!("bulk/{index:02}.flac"))
        .collect::<Vec<_>>();

    for path in &dense_paths {
        repository
            .index_source(
                source("local", path),
                metadata(path),
                &local_facts(path, None),
            )
            .await
            .unwrap();
    }
    repository
        .index_source(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                sparse.clone(),
                "only.flac".to_owned(),
            )
            .unwrap(),
            metadata("only.flac"),
            &local_facts("only.flac", None),
        )
        .await
        .unwrap();

    sqlx::query("CREATE TABLE statement_log (statements BIGINT NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO statement_log (statements) VALUES (0)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"CREATE FUNCTION log_scanned_statement() RETURNS trigger LANGUAGE plpgsql AS $fn$
        BEGIN
            UPDATE statement_log SET statements = statements + 1;
            RETURN NULL;
        END
        $fn$"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        r#"CREATE TRIGGER log_scanned_statement AFTER UPDATE ON track_sources
        FOR EACH STATEMENT EXECUTE FUNCTION log_scanned_statement()"#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let before = logged_statements(&pool).await;
    let dense_report = repository
        .reconcile_missing(&provider, &dense, &[], now)
        .await
        .unwrap();
    let dense_statements = logged_statements(&pool).await - before;

    let before = logged_statements(&pool).await;
    let sparse_report = repository
        .reconcile_missing(&provider, &sparse, &[], now)
        .await
        .unwrap();
    let sparse_statements = logged_statements(&pool).await - before;

    assert_eq!(
        dense_report,
        ReconcileReport {
            marked_missing: 40,
            reappeared: 0,
        }
    );
    assert_eq!(
        sparse_report,
        ReconcileReport {
            marked_missing: 1,
            reappeared: 0,
        }
    );
    assert_eq!(
        dense_statements, 2,
        "the mark and the clear are one statement each, however many paths are absent"
    );
    assert_eq!(
        sparse_statements, dense_statements,
        "the statement count does not follow the row count"
    );

    let before = logged_statements(&pool).await;
    let cleared = repository
        .reconcile_missing(&provider, &dense, &dense_paths, later)
        .await
        .unwrap();
    let cleared_statements = logged_statements(&pool).await - before;

    assert_eq!(
        cleared,
        ReconcileReport {
            marked_missing: 0,
            reappeared: 40,
        }
    );
    assert_eq!(
        cleared_statements, 2,
        "one statement clears every reappeared path"
    );

    let rows = repository
        .list_scanned_paths(&provider, &dense)
        .await
        .unwrap();
    assert_eq!(rows.len(), 40);
    assert!(rows.iter().all(|(_, stored)| stored.missing_at.is_none()));
}
