use std::{path::Path, sync::Arc};

use bytes::Bytes;
use md5::{Digest, Md5};

use crate::{
    features::music_library::library::{EmbeddedPicture, read_cover_art},
    features::music_library::{
        domain::{AlbumEntry, AlbumId, ArtistId, ProviderKind, SourceScope, TrackId},
        storage::contracts::{LibraryRead, SourceLookup, TrackRepositoryError},
    },
    platform::http::{self as media, FetchError as MediaFetchError},
    platform::source_path::local_path,
};

use super::{CoverArtTarget, remote};

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
    library: Option<Arc<dyn LibraryRead>>,
    sources: Option<Arc<dyn SourceLookup>>,
    client: reqwest::Client,
    host_suffixes: Vec<String>,
}

impl ArtworkService {
    pub fn new(
        library: Arc<dyn LibraryRead>,
        sources: Arc<dyn SourceLookup>,
        host_suffixes: Vec<String>,
    ) -> Self {
        let client = media::client(media::artwork_redirect_policy(host_suffixes.clone()));
        Self {
            library: Some(library),
            sources: Some(sources),
            client,
            host_suffixes,
        }
    }

    pub fn disabled() -> Self {
        let host_suffixes = super::default_host_suffixes();
        let client = media::client(media::artwork_redirect_policy(host_suffixes.clone()));
        Self {
            library: None,
            sources: None,
            client,
            host_suffixes,
        }
    }

    pub async fn load(
        &self,
        target: CoverArtTarget,
        if_none_match: Option<&str>,
        size: Option<u32>,
    ) -> Result<ArtworkOutcome, ArtworkError> {
        let Some(resolved) = self.resolve(target).await? else {
            return Ok(ArtworkOutcome::Missing);
        };

        match resolved {
            Resolved::Remote(reference) => self.fetch(&reference, if_none_match, size).await,
            Resolved::Local { path, picture } => {
                let metadata = tokio::fs::metadata(&path).await.ok();
                let etag = sized(local_etag(&path, metadata.as_ref()), size);
                if fresh(if_none_match, &etag) {
                    return Ok(ArtworkOutcome::NotModified(etag));
                }

                let (content_type, bytes) =
                    shrink(picture.content_type, Bytes::from(picture.bytes), size);

                Ok(ArtworkOutcome::Found(Artwork::new(
                    content_type,
                    bytes,
                    etag,
                )))
            }
        }
    }

    async fn resolve(&self, target: CoverArtTarget) -> Result<Option<Resolved>, ArtworkError> {
        let (Some(library), Some(sources)) = (&self.library, &self.sources) else {
            return Ok(None);
        };

        match target {
            CoverArtTarget::Track(track_id) => {
                if let Some(resolved) = self.track_artwork(library, sources, &track_id).await? {
                    return Ok(Some(resolved));
                }

                let uuid = *track_id.as_uuid();
                if let Some(resolved) = self
                    .album_artwork(library, sources, &AlbumId::from_uuid(uuid))
                    .await?
                {
                    return Ok(Some(resolved));
                }

                self.artist_artwork(library, sources, &ArtistId::from_uuid(uuid))
                    .await
            }
            CoverArtTarget::Album(album_id) => {
                self.album_artwork(library, sources, &album_id).await
            }
            CoverArtTarget::Artist(artist_id) => {
                self.artist_artwork(library, sources, &artist_id).await
            }
        }
    }

    async fn fetch(
        &self,
        reference: &str,
        if_none_match: Option<&str>,
        size: Option<u32>,
    ) -> Result<ArtworkOutcome, ArtworkError> {
        let url = reqwest::Url::parse(reference)
            .map_err(|_| ArtworkError::Fetch(MediaFetchError::DisallowedDestination))?;
        if !media::is_allowed_artwork_url(&url, &self.host_suffixes) {
            return Ok(ArtworkOutcome::Missing);
        }

        let etag = sized(etag_for(url.as_str()), size);
        if fresh(if_none_match, &etag) {
            return Ok(ArtworkOutcome::NotModified(etag));
        }

        let (content_type, bytes) = remote::fetch_artwork(&self.client, &url)
            .await
            .map_err(ArtworkError::Fetch)?;

        let (content_type, bytes) = shrink(content_type, bytes, size);

        Ok(ArtworkOutcome::Found(Artwork::new(
            content_type,
            bytes,
            etag,
        )))
    }

    async fn track_artwork(
        &self,
        library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        if let Some(local) = self.embedded_for_track(library, sources, track_id).await? {
            return Ok(Some(local));
        }

        self.stored_for_track(library, sources, track_id).await
    }

    async fn album_artwork(
        &self,
        library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        album_id: &AlbumId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let entries = self.album_entries(library, sources, album_id).await?;

        if let Some(local) = self
            .embedded_for_entries(library, sources, &entries)
            .await?
        {
            return Ok(Some(local));
        }

        self.stored_for_entries(library, sources, &entries).await
    }

    async fn artist_artwork(
        &self,
        library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        artist_id: &ArtistId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let albums = library
            .find_albums_for_artist(artist_id, None)
            .await
            .map_err(ArtworkError::Repository)?;
        let mut albums_entries = Vec::with_capacity(albums.len());
        for summary in &albums {
            albums_entries.push(
                self.album_entries(library, sources, summary.album().id())
                    .await?,
            );
        }

        for entries in &albums_entries {
            if let Some(local) = self.embedded_for_entries(library, sources, entries).await? {
                return Ok(Some(local));
            }
        }
        for entries in &albums_entries {
            if let Some(stored) = self.stored_for_entries(library, sources, entries).await? {
                return Ok(Some(stored));
            }
        }

        Ok(None)
    }

    async fn album_entries(
        &self,
        library: &Arc<dyn LibraryRead>,
        _sources: &Arc<dyn SourceLookup>,
        album_id: &AlbumId,
    ) -> Result<Vec<AlbumEntry>, ArtworkError> {
        library
            .find_album_entries(album_id, None)
            .await
            .map_err(ArtworkError::Repository)
    }

    async fn embedded_for_entries(
        &self,
        library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        entries: &[AlbumEntry],
    ) -> Result<Option<Resolved>, ArtworkError> {
        for entry in entries {
            if let Some(local) = self
                .embedded_for_track(library, sources, entry.track().id())
                .await?
            {
                return Ok(Some(local));
            }
        }

        Ok(None)
    }

    async fn stored_for_entries(
        &self,
        library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        entries: &[AlbumEntry],
    ) -> Result<Option<Resolved>, ArtworkError> {
        for entry in entries {
            if let Some(stored) = self
                .stored_for_track(library, sources, entry.track().id())
                .await?
            {
                return Ok(Some(stored));
            }
        }

        Ok(None)
    }

    async fn embedded_for_track(
        &self,
        _library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let track_sources = sources
            .find_sources(track_id)
            .await
            .map_err(ArtworkError::Repository)?;

        for source in track_sources {
            if source.provider_id().kind() != Some(ProviderKind::Local) {
                continue;
            }
            let SourceScope::Named(root) = source.source_scope() else {
                continue;
            };
            let Some(facts) = sources
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
        _library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        track_id: &TrackId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let Some((_, artwork)) = sources
            .find_artwork(track_id)
            .await
            .map_err(ArtworkError::Repository)?
        else {
            return Ok(None);
        };

        Ok(Some(Resolved::Remote(artwork.reference().to_owned())))
    }
}

fn sized(etag: String, size: Option<u32>) -> String {
    let Some(size) = size else {
        return etag;
    };

    match etag.strip_suffix('"') {
        Some(inner) => format!("{inner}-w{size}\""),
        None => format!("{etag}-w{size}"),
    }
}

fn shrink(content_type: String, bytes: Bytes, size: Option<u32>) -> (String, Bytes) {
    match size
        .and_then(|size| crate::features::music_library::artwork::thumbnail::fit(&bytes, size))
    {
        Some(thumbnail) => ("image/jpeg".to_owned(), thumbnail),
        None => (content_type, bytes),
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

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
