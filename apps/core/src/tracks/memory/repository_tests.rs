#[cfg(test)]
mod tests {

    use crate::tracks::{
        AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistCredit, ArtworkRef,
        CatalogueWrite, InMemoryTrackRepository, LibraryRead, MatchQuality, ProviderId,
        SourceIdentity, SourceLookup, SourceScope, StateStore, StateTarget, TrackId, TrackMetadata,
    };
    use uuid::Uuid;

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            SourceScope::Global,
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

    #[tokio::test]
    async fn repository_maintains_stable_bidirectional_mappings() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn two_files_of_one_recording_share_a_track() {
        let repository = InMemoryTrackRepository::default();
        let first = source("local", "album-a/01 song.flac");
        let second = source("local", "album-b/01 song.flac");

        let first_track = repository
            .get_or_create(first.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();
        let second_track = repository
            .get_or_create(second.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();

        assert_eq!(first_track.id(), second_track.id());
        assert_eq!(
            repository.find_sources(first_track.id()).await.unwrap(),
            vec![first, second]
        );
    }

    #[tokio::test]
    async fn a_moved_file_keeps_its_track() {
        let repository = InMemoryTrackRepository::default();

        let before = repository
            .get_or_create(
                source("local", "old/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();
        let after = repository
            .get_or_create(
                source("local", "new/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();

        assert_eq!(before.id(), after.id());
        assert!(
            repository
                .find_sources(before.id())
                .await
                .unwrap()
                .contains(&source("local", "new/01 song.flac")),
            "the new path is a source of the surviving track"
        );
    }

    #[tokio::test]
    async fn an_unidentified_file_keeps_its_own_track() {
        let repository = InMemoryTrackRepository::default();

        let first = repository
            .get_or_create(source("local", "a.flac"), metadata("Song"))
            .await
            .unwrap();
        let second = repository
            .get_or_create(source("local", "b.flac"), metadata("Song"))
            .await
            .unwrap();

        assert_ne!(
            first.id(),
            second.id(),
            "a file with no identifier cannot be matched to another"
        );
    }

    #[tokio::test]
    async fn gaining_an_identifier_already_held_moves_the_source() {
        let repository = InMemoryTrackRepository::default();
        let owned = source("local", "album-a/01 song.flac");

        let identified_track = repository
            .get_or_create(
                source("local", "album-b/01 song.flac"),
                identified("Song", "rec-1"),
            )
            .await
            .unwrap();
        let plain_track = repository
            .get_or_create(owned.clone(), metadata("Song"))
            .await
            .unwrap();
        assert_ne!(plain_track.id(), identified_track.id());

        let merged = repository
            .get_or_create(owned.clone(), identified("Song", "rec-1"))
            .await
            .unwrap();

        assert_eq!(merged.id(), identified_track.id());
        assert_eq!(
            repository.find_sources(merged.id()).await.unwrap(),
            vec![source("local", "album-b/01 song.flac"), owned]
        );
        assert!(
            repository
                .find_track(plain_track.id())
                .await
                .unwrap()
                .is_none(),
            "the vacated track had no other source"
        );
    }

    #[tokio::test]
    async fn an_isrc_matches_only_when_it_is_unambiguous() {
        let repository = InMemoryTrackRepository::default();

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

    #[tokio::test]
    async fn a_recording_id_outranks_an_isrc() {
        let repository = InMemoryTrackRepository::default();

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

    #[tokio::test]
    async fn repository_returns_a_stored_track_by_id() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn repository_returns_no_sources_for_an_unknown_track() {
        let repository = InMemoryTrackRepository::default();

        assert!(
            repository
                .find_sources(&TrackId::new())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn artwork_resolves_from_the_source_that_stored_it() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn recording_identifiers_round_trip_on_the_track() {
        let repository = InMemoryTrackRepository::default();
        let stored = repository
            .get_or_create(
                source("local", "recording-1"),
                metadata("Song")
                    .with_recording_id(Some("mb-recording".to_owned()))
                    .unwrap()
                    .with_isrc(Some("ISRC-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let found = repository
            .find_track(stored.id())
            .await
            .unwrap()
            .expect("stored track is found");
        assert_eq!(found.musicbrainz_recording_id(), Some("mb-recording"));
        assert_eq!(found.isrc(), Some("ISRC-1"));
    }

    fn credit(name: &str, identifier: Option<&str>) -> ArtistCredit {
        ArtistCredit::new(name.to_owned(), identifier.map(str::to_owned)).unwrap()
    }

    fn album_metadata(title: &str) -> AlbumMetadata {
        AlbumMetadata::new(title.to_owned()).unwrap()
    }

    #[tokio::test]
    async fn artist_ladder_prefers_identifiers_then_normalised_names() {
        let repository = InMemoryTrackRepository::default();

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

    #[tokio::test]
    async fn an_id_bearing_and_a_name_only_artist_coexist() {
        let repository = InMemoryTrackRepository::default();

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

    #[tokio::test]
    async fn album_ladder_matches_identifiers_then_normalised_text() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn an_identifier_attaches_to_a_text_only_album() {
        let repository = InMemoryTrackRepository::default();

        let plain = repository
            .resolve_or_create_album(&album_metadata("Discovery"))
            .await
            .unwrap();
        assert_eq!(plain.musicbrainz_release_group_id(), None);

        let discovered = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-1".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(discovered.id(), plain.id());
        assert_eq!(discovered.musicbrainz_release_group_id(), Some("rg-1"));
        assert_eq!(
            repository
                .find_album(plain.id())
                .await
                .unwrap()
                .unwrap()
                .musicbrainz_release_group_id(),
            Some("rg-1")
        );
    }

    #[tokio::test]
    async fn two_identifiers_are_two_albums_even_when_the_text_agrees() {
        let repository = InMemoryTrackRepository::default();

        let original = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-original".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let remaster = repository
            .resolve_or_create_album(
                &album_metadata("Discovery")
                    .with_release_group_id(Some("rg-remaster".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(original.id(), remaster.id());
    }

    #[tokio::test]
    async fn album_sources_and_membership_round_trip() {
        let repository = InMemoryTrackRepository::default();
        let album = repository
            .resolve_or_create_album(&album_metadata("Discovery"))
            .await
            .unwrap();
        let track = repository
            .get_or_create(source("local", "path-a"), metadata("Song"))
            .await
            .unwrap();
        let album_source = source("youtube_music", "album-1");
        let membership = AlbumTrack::new(Some(1), Some(3), 2).unwrap();

        repository
            .set_album_source(album.id(), &album_source)
            .await
            .unwrap();
        repository
            .set_album_source(album.id(), &album_source)
            .await
            .unwrap();
        assert_eq!(
            repository.find_album_sources(album.id()).await.unwrap(),
            vec![album_source]
        );

        repository
            .set_album_track(album.id(), track.id(), &membership)
            .await
            .unwrap();
        let entries = repository
            .find_album_entries(album.id(), None)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].track().id(), track.id());
        assert_eq!(entries[0].membership(), &membership);
        assert_eq!(
            repository
                .find_album_for_track(track.id())
                .await
                .unwrap()
                .expect("the track is filed under the album")
                .id(),
            album.id()
        );

        assert!(
            repository
                .set_album_track(
                    &AlbumId::new(),
                    track.id(),
                    &AlbumTrack::new(None, None, 0).unwrap(),
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn re_filing_a_track_replaces_its_membership() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn album_entries_are_ordered_and_totalled() {
        let repository = InMemoryTrackRepository::default();
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

        let third = repository
            .get_or_create(source("local", "path-c"), metadata("Third"))
            .await
            .unwrap();
        let first = repository
            .get_or_create(source("local", "path-a"), metadata("First"))
            .await
            .unwrap();
        let second = repository
            .get_or_create(source("local", "path-b"), metadata("Second"))
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
        assert_eq!(artists.len(), 1);
        assert_eq!(artists[0].artist().id(), artist.id());
        assert_eq!(artists[0].album_count(), 1);
    }

    #[tokio::test]
    async fn search_matches_titles_and_artists_and_ranks_owned_first() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn search_ranks_match_quality_before_ownership() {
        let repository = InMemoryTrackRepository::default();
        let exact = repository
            .get_or_create(
                source("local", "path-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let prefix = repository
            .get_or_create(
                source("local", "path-prefix"),
                TrackMetadata::from_names(
                    "Zebra Crossing".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let word = repository
            .get_or_create(
                source("youtube_music", "vid-word"),
                TrackMetadata::from_names(
                    "The Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
        let ordered = hits
            .iter()
            .map(|hit| (hit.track().id().clone(), hit.match_quality()))
            .collect::<Vec<_>>();

        assert_eq!(
            ordered,
            vec![
                (exact.id().clone(), Some(MatchQuality::Exact)),
                (prefix.id().clone(), Some(MatchQuality::Prefix)),
                (word.id().clone(), Some(MatchQuality::WordBoundary)),
            ],
            "an owned prefix does not outrank a remote word-boundary match"
        );
    }

    #[tokio::test]
    async fn search_uses_ownership_inside_a_tier() {
        let repository = InMemoryTrackRepository::default();
        let remote = repository
            .get_or_create(
                source("youtube_music", "vid-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let owned = repository
            .get_or_create(
                source("local", "path-exact"),
                TrackMetadata::from_names(
                    "Zebra".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        let hits = repository.search_tracks("zebra", 10, None).await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].track().id(), owned.id());
        assert_eq!(hits[1].track().id(), remote.id());
    }

    #[tokio::test]
    async fn search_does_not_match_inside_a_word() {
        let repository = InMemoryTrackRepository::default();
        repository
            .get_or_create(
                source("local", "path-american"),
                TrackMetadata::from_names(
                    "all-american bitch".to_owned(),
                    vec!["Olivia Rodrigo".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();

        assert!(
            repository
                .search_tracks("me", 10, None)
                .await
                .unwrap()
                .is_empty(),
            "a short token must not match inside american"
        );
        assert!(
            repository
                .search_tracks("american", 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            repository
                .search_tracks("bitch", 10, None)
                .await
                .unwrap()
                .len(),
            1,
            "a whole word still matches"
        );
    }

    #[tokio::test]
    async fn search_artists_collapses_rows_sharing_a_name() {
        let repository = InMemoryTrackRepository::default();
        let identified = repository
            .resolve_or_create_artist(&credit("Cage the Elephant", Some("mb-1")))
            .await
            .unwrap();
        repository
            .resolve_or_create_artist(&credit("Cage The Elephant", None))
            .await
            .unwrap();

        let artists = repository.search_artists("cage", 10, None).await.unwrap();
        assert_eq!(artists.len(), 1, "the two rows present as one result");
        assert_eq!(artists[0].artist().id(), identified.id());
        assert!(artists[0].artist().musicbrainz_artist_id().is_some());
    }

    #[tokio::test]
    async fn candidate_tracks_batches_normalized_title_lookups() {
        let repository = InMemoryTrackRepository::default();
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
                TrackMetadata::from_names(
                    "Other".to_owned(),
                    vec!["Other".to_owned()],
                    Some(1_000),
                )
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

    #[tokio::test]
    async fn search_matches_albums_and_their_artists() {
        let repository = InMemoryTrackRepository::default();
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

    #[tokio::test]
    async fn albums_order_by_title_artist_and_creation() {
        let repository = InMemoryTrackRepository::default();
        let first = repository
            .resolve_or_create_album(
                &album_metadata("Zebra").with_album_artist(credit("A Band", None)),
            )
            .await
            .unwrap();
        let second = repository
            .resolve_or_create_album(
                &album_metadata("Apple").with_album_artist(credit("Z Band", None)),
            )
            .await
            .unwrap();

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

    #[tokio::test]
    async fn per_user_state_round_trips_for_every_entity() {
        let repository = InMemoryTrackRepository::default();
        let user = Uuid::now_v7();
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

    #[tokio::test]
    async fn album_orders_reflect_plays_and_ratings() {
        let repository = InMemoryTrackRepository::default();
        let user = Uuid::now_v7();
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

    #[tokio::test]
    async fn genres_aggregate_and_filter_albums() {
        let repository = InMemoryTrackRepository::default();
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
}
