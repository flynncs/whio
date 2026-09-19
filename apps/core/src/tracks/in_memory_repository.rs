use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;

use crate::tracks::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, ArtworkRef,
    ProviderId, SourceFileFacts, Track, TrackMetadata, domain::normalize_for_matching,
};

use super::{SourceIdentity, SourceScope, TrackId, TrackRepository, TrackRepositoryError};

struct TrackEntry {
    track: Track,
    sources: Vec<SourceIdentity>,
    album_source: Option<String>,
}

#[derive(Default)]
struct TrackIndexes {
    by_source: HashMap<SourceIdentity, TrackId>,
    by_track_id: HashMap<TrackId, TrackEntry>,
    artwork_by_source: HashMap<SourceIdentity, ArtworkRef>,
    file_facts_by_source: HashMap<SourceIdentity, SourceFileFacts>,
    artists_by_id: HashMap<ArtistId, Artist>,
    artist_by_identifier: HashMap<String, ArtistId>,
    artist_by_name: HashMap<String, ArtistId>,
    albums_by_id: HashMap<AlbumId, Album>,
    album_by_release_group: HashMap<String, AlbumId>,
    album_by_release: HashMap<String, AlbumId>,
    album_by_text: HashMap<(Option<ArtistId>, String), AlbumId>,
    album_sources: HashMap<AlbumId, Vec<SourceIdentity>>,
    album_tracks: HashMap<AlbumId, Vec<(TrackId, AlbumTrack)>>,
}

#[derive(Default)]
pub struct InMemoryTrackRepository {
    indexes: Mutex<TrackIndexes>,
}

#[async_trait]
impl TrackRepository for InMemoryTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if let Some(track_id) = indexes.by_source.get(&source).cloned() {
            let entry = indexes
                .by_track_id
                .get_mut(&track_id)
                .ok_or(TrackRepositoryError::Unavailable)?;

            let album = merged_album(source.provider_id(), metadata.album(), entry);
            if album.changed {
                entry.album_source = Some(source.provider_id().as_str().to_owned());
            }

            // latest observation wins for display metadata; the album follows
            // provider precedence.
            let metadata = metadata
                .with_album(album.value)
                .map_err(|_| TrackRepositoryError::InvalidData)?;
            entry.track = Track::new(track_id.clone(), metadata);

            return Ok(entry.track.clone());
        }

        let track_id = TrackId::new();
        let album_source = metadata
            .album()
            .map(|_| source.provider_id().as_str().to_owned());

        let track = Track::new(track_id.clone(), metadata);
        let entry = TrackEntry {
            track: track.clone(),
            sources: vec![source.clone()],
            album_source,
        };

        indexes.by_source.insert(source, track_id.clone());
        indexes.by_track_id.insert(track_id, entry);

        return Ok(track);
    }

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.sources.clone())
            .unwrap_or_default())
    }

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.track.clone()))
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .artwork_by_source
            .insert(source.clone(), artwork.clone());
        Ok(())
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some(entry) = indexes.by_track_id.get(track_id) else {
            return Ok(None);
        };

        Ok(entry
            .sources
            .iter()
            .filter_map(|source| {
                indexes
                    .artwork_by_source
                    .get(source)
                    .map(|artwork| (source, artwork))
            })
            .min_by_key(|(source, _)| source.provider_id().precedence())
            .map(|(source, artwork)| (source.clone(), artwork.clone())))
    }

    async fn set_source_file_facts(
        &self,
        source: &SourceIdentity,
        facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .file_facts_by_source
            .insert(source.clone(), facts.clone());
        Ok(())
    }

    async fn find_source_file_facts(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.file_facts_by_source.get(source).cloned())
    }

    async fn list_scanned_paths(
        &self,
        provider_id: &ProviderId,
        source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .file_facts_by_source
            .iter()
            .filter(|(source, _)| {
                source.provider_id() == provider_id && source.source_scope() == source_scope
            })
            .map(|(source, facts)| (source.clone(), facts.clone()))
            .collect())
    }

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(resolve_artist(&mut indexes, credit))
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let album_artist_id = metadata
            .album_artist()
            .map(|credit| resolve_artist(&mut indexes, credit).id().clone());

        Ok(resolve_album(&mut indexes, metadata, album_artist_id))
    }

    async fn set_album_source(
        &self,
        album_id: &AlbumId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id) {
            return Err(TrackRepositoryError::InvalidData);
        }

        let sources = indexes.album_sources.entry(album_id.clone()).or_default();
        if !sources.contains(source) {
            sources.push(source.clone());
        }

        Ok(())
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id)
            || !indexes.by_track_id.contains_key(track_id)
        {
            return Err(TrackRepositoryError::InvalidData);
        }

        let tracks = indexes.album_tracks.entry(album_id.clone()).or_default();
        match tracks.iter_mut().find(|(id, _)| id == track_id) {
            Some(entry) => entry.1 = *membership,
            None => tracks.push((track_id.clone(), *membership)),
        }

        Ok(())
    }

    async fn find_album(&self, album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.albums_by_id.get(album_id).cloned())
    }

    async fn find_album_sources(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .album_sources
            .get(album_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn find_album_tracks(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<(TrackId, AlbumTrack)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .album_tracks
            .get(album_id)
            .cloned()
            .unwrap_or_default())
    }
}

fn resolve_artist(indexes: &mut TrackIndexes, credit: &ArtistCredit) -> Artist {
    if let Some(identifier) = credit.musicbrainz_artist_id()
        && let Some(artist_id) = indexes.artist_by_identifier.get(identifier)
        && let Some(artist) = indexes.artists_by_id.get(artist_id)
    {
        return artist.clone();
    }

    let normalized = normalize_for_matching(credit.name());

    if let Some(artist_id) = indexes.artist_by_name.get(&normalized).cloned() {
        let Some(artist) = indexes.artists_by_id.get(&artist_id).cloned() else {
            unreachable!("a name index only points at a stored artist");
        };

        let Some(identifier) = credit.musicbrainz_artist_id() else {
            return artist;
        };

        let attached = Artist::new(
            artist.id().clone(),
            artist.name().to_owned(),
            artist.sort_name().map(str::to_owned),
            Some(identifier.to_owned()),
        )
        .expect("a stored artist stays valid when an identifier is attached");

        indexes
            .artist_by_identifier
            .insert(identifier.to_owned(), attached.id().clone());
        indexes.artist_by_name.remove(&normalized);
        indexes
            .artists_by_id
            .insert(attached.id().clone(), attached.clone());

        return attached;
    }

    let artist = Artist::new(
        ArtistId::new(),
        credit.name().to_owned(),
        None,
        credit.musicbrainz_artist_id().map(str::to_owned),
    )
    .expect("a validated credit yields a valid artist");

    if let Some(identifier) = artist.musicbrainz_artist_id() {
        indexes
            .artist_by_identifier
            .insert(identifier.to_owned(), artist.id().clone());
    } else {
        indexes
            .artist_by_name
            .insert(normalized, artist.id().clone());
    }
    indexes
        .artists_by_id
        .insert(artist.id().clone(), artist.clone());

    artist
}

fn resolve_album(
    indexes: &mut TrackIndexes,
    metadata: &AlbumMetadata,
    album_artist_id: Option<ArtistId>,
) -> Album {
    if let Some(identifier) = metadata.musicbrainz_release_group_id()
        && let Some(album_id) = indexes.album_by_release_group.get(identifier)
        && let Some(album) = indexes.albums_by_id.get(album_id)
    {
        return album.clone();
    }

    if let Some(identifier) = metadata.musicbrainz_release_id()
        && let Some(album_id) = indexes.album_by_release.get(identifier)
        && let Some(album) = indexes.albums_by_id.get(album_id)
    {
        return album.clone();
    }

    let normalized = normalize_for_matching(metadata.title());
    let key = (album_artist_id.clone(), normalized);

    if let Some(album_id) = indexes.album_by_text.get(&key).cloned()
        && let Some(existing) = indexes.albums_by_id.get(&album_id).cloned()
    {
        let existing_has_identifier = existing.musicbrainz_release_group_id().is_some()
            || existing.musicbrainz_release_id().is_some();
        let incoming_has_identifier = metadata.musicbrainz_release_group_id().is_some()
            || metadata.musicbrainz_release_id().is_some();

        let both_identified = existing_has_identifier && incoming_has_identifier;

        if !both_identified {
            if !incoming_has_identifier {
                return existing;
            }

            let attached = Album::new(
                existing.id().clone(),
                existing.title().to_owned(),
                album_artist_id,
                metadata.year().or(existing.year()),
                preferred(metadata.release_type(), existing.release_type()),
                preferred(metadata.label(), existing.label()),
                preferred(
                    metadata.musicbrainz_release_group_id(),
                    existing.musicbrainz_release_group_id(),
                ),
                preferred(
                    metadata.musicbrainz_release_id(),
                    existing.musicbrainz_release_id(),
                ),
            )
            .expect("attaching an identifier keeps the album valid");

            if let Some(identifier) = attached.musicbrainz_release_group_id() {
                indexes
                    .album_by_release_group
                    .insert(identifier.to_owned(), attached.id().clone());
            }
            if let Some(identifier) = attached.musicbrainz_release_id() {
                indexes
                    .album_by_release
                    .insert(identifier.to_owned(), attached.id().clone());
            }
            indexes
                .albums_by_id
                .insert(attached.id().clone(), attached.clone());

            return attached;
        }
    }

    let album = Album::new(
        AlbumId::new(),
        metadata.title().to_owned(),
        album_artist_id,
        metadata.year(),
        metadata.release_type().map(str::to_owned),
        metadata.label().map(str::to_owned),
        metadata.musicbrainz_release_group_id().map(str::to_owned),
        metadata.musicbrainz_release_id().map(str::to_owned),
    )
    .expect("validated album metadata yields a valid album");

    indexes
        .album_by_text
        .entry(key)
        .or_insert(album.id().clone());
    if let Some(identifier) = album.musicbrainz_release_group_id() {
        indexes
            .album_by_release_group
            .insert(identifier.to_owned(), album.id().clone());
    }
    if let Some(identifier) = album.musicbrainz_release_id() {
        indexes
            .album_by_release
            .insert(identifier.to_owned(), album.id().clone());
    }
    indexes
        .albums_by_id
        .insert(album.id().clone(), album.clone());

    album
}

fn preferred<'a>(incoming: Option<&'a str>, existing: Option<&'a str>) -> Option<String> {
    incoming.or(existing).map(str::to_owned)
}

struct MergedAlbum {
    value: Option<String>,
    changed: bool,
}

fn merged_album(
    incoming_provider: &ProviderId,
    incoming: Option<&str>,
    entry: &TrackEntry,
) -> MergedAlbum {
    let Some(incoming) = incoming else {
        return MergedAlbum {
            value: entry.track.metadata().album().map(str::to_owned),
            changed: false,
        };
    };

    let existing_rank = entry
        .album_source
        .as_deref()
        .and_then(|source| ProviderId::new(source.to_owned()).ok())
        .map(|provider| provider.precedence());

    let current = entry.track.metadata().album().map(str::to_owned);

    // a higher-precedence provider replaces the album; within one provider the
    // fresher observation wins.
    match current {
        Some(current)
            if existing_rank.is_some_and(|rank| rank < incoming_provider.precedence()) =>
        {
            MergedAlbum {
                value: Some(current),
                changed: false,
            }
        }
        _ => MergedAlbum {
            value: Some(incoming.to_owned()),
            changed: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{SourceScope, TrackMetadata};

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::new(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
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
    async fn album_is_stored_and_refreshed_within_a_provider() {
        let repository = InMemoryTrackRepository::default();
        let youtube = source("youtube_music", "vid-1");

        let first = repository
            .get_or_create(
                youtube.clone(),
                metadata("Song")
                    .with_album(Some("First".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.album(), Some("First"));

        let refreshed = repository
            .get_or_create(
                youtube,
                metadata("Song")
                    .with_album(Some("Corrected".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(refreshed.id(), first.id());
        assert_eq!(refreshed.album(), Some("Corrected"));

        let stored = repository
            .find_track(first.id())
            .await
            .unwrap()
            .expect("stored track is found");
        assert_eq!(stored.album(), Some("Corrected"));
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
        assert_eq!(
            repository.find_album_tracks(album.id()).await.unwrap(),
            vec![(track.id().clone(), membership)]
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
}
