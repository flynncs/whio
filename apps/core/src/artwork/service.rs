use std::{path::Path, sync::Arc};

use bytes::Bytes;
use md5::{Digest, Md5};

use crate::{
    library::{EmbeddedPicture, read_cover_art},
    media::{self, MediaFetchError},
    source_path::local_path,
    tracks::{
        AlbumEntry, AlbumId, ArtistId, ProviderKind, SourceScope, TrackId, TrackRepository,
        TrackRepositoryError,
    },
};

use super::CoverArtTarget;

#[derive(Debug, thiserror::Error)]
pub enum ArtworkError {
    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("artwork fetch failed")]
    Fetch(#[source] MediaFetchError),

    #[error("artwork could not be read")]
    Unavailable,
}

pub enum ArtworkOutcome {
    Found(Artwork),
    NotModified(String),
    Missing,
}

pub struct Artwork {
    content_type: String,
    bytes: Bytes,
    etag: String,
}

impl Artwork {
    fn new(content_type: String, bytes: Bytes, etag: String) -> Self {
        Self {
            content_type,
            bytes,
            etag,
        }
    }

    pub fn into_parts(self) -> (String, Bytes, String) {
        (self.content_type, self.bytes, self.etag)
    }
}

enum Resolved {
    Remote(String),
    Local {
        path: std::path::PathBuf,
        picture: EmbeddedPicture,
    },
}

pub struct ArtworkService {
    repository: Option<Arc<dyn TrackRepository>>,
    client: reqwest::Client,
    host_suffixes: Vec<String>,
}

impl ArtworkService {
    pub fn new(repository: Arc<dyn TrackRepository>, host_suffixes: Vec<String>) -> Self {
        let client = media::client(media::artwork_redirect_policy(host_suffixes.clone()));
        Self {
            repository: Some(repository),
            client,
            host_suffixes,
        }
    }

    pub fn disabled() -> Self {
        let host_suffixes = media::default_artwork_host_suffixes();
        let client = media::client(media::artwork_redirect_policy(host_suffixes.clone()));
        Self {
            repository: None,
            client,
            host_suffixes,
        }
    }

    pub async fn load(
        &self,
        target: CoverArtTarget,
        if_none_match: Option<&str>,
    ) -> Result<ArtworkOutcome, ArtworkError> {
        let Some(resolved) = self.resolve(target).await? else {
            return Ok(ArtworkOutcome::Missing);
        };

        match resolved {
            Resolved::Remote(reference) => self.fetch(&reference, if_none_match).await,
            Resolved::Local { path, picture } => {
                let metadata = tokio::fs::metadata(&path).await.ok();
                let etag = local_etag(&path, metadata.as_ref());
                if fresh(if_none_match, &etag) {
                    return Ok(ArtworkOutcome::NotModified(etag));
                }

                Ok(ArtworkOutcome::Found(Artwork::new(
                    picture.content_type,
                    Bytes::from(picture.bytes),
                    etag,
                )))
            }
        }
    }

    async fn resolve(&self, target: CoverArtTarget) -> Result<Option<Resolved>, ArtworkError> {
        let Some(repository) = &self.repository else {
            return Ok(None);
        };

        match target {
            CoverArtTarget::Track(track_id) => self.track_artwork(repository, &track_id).await,
            CoverArtTarget::Album(album_id) => self.album_artwork(repository, &album_id).await,
            CoverArtTarget::Artist(artist_id) => self.artist_artwork(repository, &artist_id).await,
        }
    }

    async fn fetch(
        &self,
        reference: &str,
        if_none_match: Option<&str>,
    ) -> Result<ArtworkOutcome, ArtworkError> {
        let url = reqwest::Url::parse(reference)
            .map_err(|_| ArtworkError::Fetch(MediaFetchError::DisallowedDestination))?;
        if !media::is_allowed_artwork_url(&url, &self.host_suffixes) {
            return Ok(ArtworkOutcome::Missing);
        }

        let etag = etag_for(url.as_str());
        if fresh(if_none_match, &etag) {
            return Ok(ArtworkOutcome::NotModified(etag));
        }

        let (content_type, bytes) = media::fetch_artwork(&self.client, &url)
            .await
            .map_err(ArtworkError::Fetch)?;

        Ok(ArtworkOutcome::Found(Artwork::new(
            content_type,
            bytes,
            etag,
        )))
    }

    async fn track_artwork(
        &self,
        repository: &Arc<dyn TrackRepository>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        if let Some(local) = self.embedded_for_track(repository, track_id).await? {
            return Ok(Some(local));
        }

        self.stored_for_track(repository, track_id).await
    }

    async fn album_artwork(
        &self,
        repository: &Arc<dyn TrackRepository>,
        album_id: &AlbumId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let entries = self.album_entries(repository, album_id).await?;

        if let Some(local) = self.embedded_for_entries(repository, &entries).await? {
            return Ok(Some(local));
        }

        self.stored_for_entries(repository, &entries).await
    }

    async fn artist_artwork(
        &self,
        repository: &Arc<dyn TrackRepository>,
        artist_id: &ArtistId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let albums = repository
            .find_albums_for_artist(artist_id, None)
            .await
            .map_err(ArtworkError::Repository)?;
        let mut albums_entries = Vec::with_capacity(albums.len());
        for summary in &albums {
            albums_entries.push(self.album_entries(repository, summary.album().id()).await?);
        }

        for entries in &albums_entries {
            if let Some(local) = self.embedded_for_entries(repository, entries).await? {
                return Ok(Some(local));
            }
        }
        for entries in &albums_entries {
            if let Some(stored) = self.stored_for_entries(repository, entries).await? {
                return Ok(Some(stored));
            }
        }

        Ok(None)
    }

    async fn album_entries(
        &self,
        repository: &Arc<dyn TrackRepository>,
        album_id: &AlbumId,
    ) -> Result<Vec<AlbumEntry>, ArtworkError> {
        repository
            .find_album_entries(album_id, None)
            .await
            .map_err(ArtworkError::Repository)
    }

    async fn embedded_for_entries(
        &self,
        repository: &Arc<dyn TrackRepository>,
        entries: &[AlbumEntry],
    ) -> Result<Option<Resolved>, ArtworkError> {
        for entry in entries {
            if let Some(local) = self
                .embedded_for_track(repository, entry.track().id())
                .await?
            {
                return Ok(Some(local));
            }
        }

        Ok(None)
    }

    async fn stored_for_entries(
        &self,
        repository: &Arc<dyn TrackRepository>,
        entries: &[AlbumEntry],
    ) -> Result<Option<Resolved>, ArtworkError> {
        for entry in entries {
            if let Some(stored) = self
                .stored_for_track(repository, entry.track().id())
                .await?
            {
                return Ok(Some(stored));
            }
        }

        Ok(None)
    }

    async fn embedded_for_track(
        &self,
        repository: &Arc<dyn TrackRepository>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let sources = repository
            .find_sources(track_id)
            .await
            .map_err(ArtworkError::Repository)?;

        for source in sources {
            if source.provider_id().kind() != Some(ProviderKind::Local) {
                continue;
            }
            let SourceScope::Named(root) = source.source_scope() else {
                continue;
            };
            let Some(facts) = repository
                .find_source_file_facts(&source)
                .await
                .map_err(ArtworkError::Repository)?
            else {
                continue;
            };
            if facts.missing_at.is_some() {
                continue;
            }
            let Some(relative) = facts.path.as_deref() else {
                continue;
            };
            let Some(path) = local_path(root, relative) else {
                continue;
            };

            let read = tokio::task::spawn_blocking({
                let path = path.clone();
                move || read_cover_art(&path)
            })
            .await
            .map_err(|_| ArtworkError::Unavailable)?;

            match read {
                Ok(Some(picture)) => return Ok(Some(Resolved::Local { path, picture })),
                Ok(None) => continue,
                Err(error) => {
                    tracing::debug!(%error, path = %path.display(), "embedded cover art could not be read");
                    continue;
                }
            }
        }

        Ok(None)
    }

    async fn stored_for_track(
        &self,
        repository: &Arc<dyn TrackRepository>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let Some((_, artwork)) = repository
            .find_artwork(track_id)
            .await
            .map_err(ArtworkError::Repository)?
        else {
            return Ok(None);
        };

        Ok(Some(Resolved::Remote(artwork.reference().to_owned())))
    }
}

fn fresh(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(header) = if_none_match else {
        return false;
    };

    header.split(',').any(|tag| {
        let tag = tag.trim();
        tag == "*" || tag.trim_start_matches("W/") == etag
    })
}

fn etag_for(value: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(value.as_bytes());

    format!("\"{:x}\"", hasher.finalize())
}

fn local_etag(path: &Path, metadata: Option<&std::fs::Metadata>) -> String {
    let mut basis = format!("{}:", path.display());
    if let Some(metadata) = metadata {
        basis.push_str(&metadata.len().to_string());
        if let Ok(modified) = metadata.modified() {
            basis.push(':');
            basis.push_str(&chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339());
        }
    }

    etag_for(&basis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{
        AlbumMetadata, AlbumTrack, ArtistCredit, ArtistId, ArtworkRef, InMemoryTrackRepository,
        ProviderId, SourceFileFacts, SourceIdentity, TrackId, TrackMetadata,
    };

    fn pictured_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("whio-artwork-{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        crate::library::write_pictured_flac(
            &root.join("a.flac"),
            vec![lofty::picture::Picture::new_unchecked(
                lofty::picture::PictureType::CoverFront,
                Some(lofty::picture::MimeType::Jpeg),
                None,
                vec![0xFF, 0xD8, 0xFF, 0xE0],
            )],
        );

        root
    }

    fn plain_root(label: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("whio-artwork-{label}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac"),
            root.join("a.flac"),
        )
        .unwrap();

        root
    }

    fn local_source(root: &std::path::Path, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named(root.display().to_string()),
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn remote_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn local_facts(relative: &str) -> SourceFileFacts {
        SourceFileFacts {
            path: Some(relative.to_owned()),
            size_bytes: 10,
            modified_at: None,
            missing_at: None,
            content_type: None,
            suffix: Some("flac".to_owned()),
            acoustid_fingerprint: None,
        }
    }

    fn track_metadata(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    async fn add_local_track(
        repository: &Arc<InMemoryTrackRepository>,
        root: &std::path::Path,
        external_id: &str,
    ) -> TrackId {
        let source = local_source(root, external_id);
        let track = repository
            .get_or_create(source.clone(), track_metadata("Local"))
            .await
            .unwrap();
        repository
            .set_source_file_facts(&source, &local_facts("a.flac"))
            .await
            .unwrap();

        track.id().clone()
    }

    async fn add_stored_remote_track(
        repository: &Arc<InMemoryTrackRepository>,
        external_id: &str,
    ) -> TrackId {
        let source = remote_source(external_id);
        let track = repository
            .get_or_create(source.clone(), track_metadata("Remote"))
            .await
            .unwrap();
        repository
            .set_source_artwork(
                &source,
                &ArtworkRef::url("https://art.test/remote.jpg".to_owned()).unwrap(),
            )
            .await
            .unwrap();

        track.id().clone()
    }

    async fn album_with(
        repository: &Arc<InMemoryTrackRepository>,
        album_artist: Option<&str>,
        members: &[TrackId],
    ) -> AlbumId {
        let mut metadata = AlbumMetadata::new("Album".to_owned()).unwrap();
        if let Some(name) = album_artist {
            metadata =
                metadata.with_album_artist(ArtistCredit::new(name.to_owned(), None).unwrap());
        }
        let album = repository.resolve_or_create_album(&metadata).await.unwrap();
        for (index, track_id) in members.iter().enumerate() {
            repository
                .set_album_track(
                    album.id(),
                    track_id,
                    &AlbumTrack::new(Some(1), Some(index as u16 + 1), index as u32).unwrap(),
                )
                .await
                .unwrap();
        }

        album.id().clone()
    }

    fn service(repository: Arc<InMemoryTrackRepository>) -> ArtworkService {
        ArtworkService::new(repository, media::default_artwork_host_suffixes())
    }

    #[tokio::test]
    async fn an_embedded_picture_is_a_tracks_cover_art() {
        let root = pictured_root("track");
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = add_local_track(&repository, &root, "recording-1").await;

        let resolved = service(repository)
            .resolve(CoverArtTarget::Track(track_id))
            .await
            .unwrap()
            .expect("embedded art");

        assert!(matches!(resolved, Resolved::Local { .. }));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn album_art_walks_members_and_falls_back_to_stored_artwork() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let root = plain_root("plain");
        let plain = add_local_track(&repository, &root, "recording-plain").await;
        let remote = add_stored_remote_track(&repository, "vid-1").await;
        let album = album_with(&repository, None, &[plain, remote]).await;

        let resolved = service(repository)
            .resolve(CoverArtTarget::Album(album))
            .await
            .unwrap()
            .expect("stored remote art");

        assert!(matches!(
            resolved,
            Resolved::Remote(reference) if reference == "https://art.test/remote.jpg"
        ));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn album_art_prefers_embedded_art_over_an_earlier_stored_thumbnail() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let remote = add_stored_remote_track(&repository, "vid-1").await;
        let root = pictured_root("prefer-local");
        let local = add_local_track(&repository, &root, "recording-1").await;
        let album = album_with(&repository, None, &[remote, local]).await;

        let resolved = service(repository)
            .resolve(CoverArtTarget::Album(album))
            .await
            .unwrap()
            .expect("embedded art");

        assert!(matches!(resolved, Resolved::Local { .. }));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn artist_art_comes_from_the_first_album_with_art() {
        let root = pictured_root("artist");
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = add_local_track(&repository, &root, "recording-1").await;
        let artist = repository
            .resolve_or_create_artist(&ArtistCredit::new("Artist".to_owned(), None).unwrap())
            .await
            .unwrap();
        album_with(&repository, Some("Artist"), &[track_id]).await;

        let resolved = service(repository)
            .resolve(CoverArtTarget::Artist(artist.id().clone()))
            .await
            .unwrap()
            .expect("album art");

        assert!(matches!(resolved, Resolved::Local { .. }));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn artwork_is_absent_without_pictures_or_stored_artwork() {
        let service = service(Arc::new(InMemoryTrackRepository::default()));

        assert!(
            service
                .resolve(CoverArtTarget::Album(AlbumId::new()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            service
                .resolve(CoverArtTarget::Artist(ArtistId::new()))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_local_picture_is_served_with_a_stable_etag() {
        let root = pictured_root("etag");
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = add_local_track(&repository, &root, "recording-1").await;
        let service = service(repository);

        let ArtworkOutcome::Found(artwork) = service
            .load(CoverArtTarget::Track(track_id.clone()), None)
            .await
            .unwrap()
        else {
            panic!("expected found artwork");
        };
        let (content_type, bytes, etag) = artwork.into_parts();
        assert_eq!(content_type, "image/jpeg");
        assert_eq!(bytes.as_ref(), &[0xFF, 0xD8, 0xFF, 0xE0]);

        assert!(matches!(
            service
                .load(CoverArtTarget::Track(track_id), Some(&etag))
                .await
                .unwrap(),
            ArtworkOutcome::NotModified(returned) if returned == etag
        ));

        std::fs::remove_dir_all(&root).unwrap();
    }
}
