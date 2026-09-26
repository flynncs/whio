use super::PostgresTrackRepository;
use crate::features::music_library::{
    domain::{
        AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistCredit, ArtworkRef,
        ProviderId, SourceIdentity, SourceScope, StateTarget, TrackId, TrackMetadata,
    },
    storage::contracts::{CatalogueWrite, LibraryRead, SourceLookup, StateStore},
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
            metadata("One").with_genre(Some("Rock".to_owned())).unwrap(),
        )
        .await
        .unwrap();
    let pop = repository
        .get_or_create(
            source("local", "path-b"),
            metadata("Two").with_genre(Some("Pop".to_owned())).unwrap(),
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
