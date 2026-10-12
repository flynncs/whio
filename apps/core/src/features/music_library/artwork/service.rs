use std::{path::Path, sync::Arc};

use bytes::Bytes;
use md5::{Digest, Md5};

use crate::{
    features::music_library::library::{
        EmbeddedPicture, LOCAL_MAX_PICTURE_BYTES, TagError, read_cover_art, sniff_picture,
    },
    features::music_library::{
        domain::{
            AlbumEntry, AlbumId, ArtistId, ProviderKind, SourceIdentity, SourceScope, TrackId,
        },
        storage::contracts::{LibraryRead, SourceLookup, TrackRepositoryError},
    },
    platform::http::{self as media, FetchError as MediaFetchError},
    platform::source_path::local_path,
};

use super::{
    ArtworkCache, ArtworkRole, CoverArtTarget, FetchedImage, PortraitGate, PromotionOutcome,
    RefusalReason, remote,
};

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
    fallback: Option<String>,
}

impl Artwork {
    fn new(content_type: String, bytes: Bytes, etag: String) -> Self {
        Self {
            content_type,
            bytes,
            etag,
            fallback: None,
        }
    }

    fn labelled(content_type: String, bytes: Bytes, etag: String, label: String) -> Self {
        Self {
            content_type,
            bytes,
            etag,
            fallback: Some(label),
        }
    }

    pub fn fallback(&self) -> Option<&str> {
        self.fallback.as_deref()
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
    Ready {
        bytes: Bytes,
        content_type: String,
        etag: String,
    },
    Fallback {
        label: String,
        bytes: Bytes,
        content_type: String,
        etag: String,
    },
}

pub struct ArtworkService {
    library: Option<Arc<dyn LibraryRead>>,
    sources: Option<Arc<dyn SourceLookup>>,
    client: reqwest::Client,
    host_suffixes: Vec<String>,
    cache: Option<Arc<ArtworkCache>>,
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
            cache: None,
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
            cache: None,
        }
    }

    pub fn with_cache(mut self, cache: Arc<ArtworkCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn with_host_suffixes(mut self, host_suffixes: Vec<String>) -> Self {
        self.client = media::client(media::artwork_redirect_policy(host_suffixes.clone()));
        self.host_suffixes = host_suffixes;
        self
    }

    pub async fn promote_artist_portrait(
        &self,
        artist_id: &ArtistId,
        source: &SourceIdentity,
        reference: &str,
        gate: PortraitGate,
    ) -> Result<PromotionOutcome, ArtworkError> {
        if gate == PortraitGate::ReferenceOnly {
            return Ok(PromotionOutcome::Refused(RefusalReason::NotServable));
        }

        let Some(cache) = &self.cache else {
            return Ok(PromotionOutcome::Refused(RefusalReason::NoCacheDir));
        };

        let url = reqwest::Url::parse(reference)
            .map_err(|_| ArtworkError::Fetch(MediaFetchError::DisallowedDestination))?;
        if !media::is_allowed_artwork_url(&url, &self.host_suffixes) {
            return Ok(PromotionOutcome::Refused(
                RefusalReason::DisallowedReference,
            ));
        }

        let (content_type, bytes) = remote::fetch_artwork(&self.client, &url)
            .await
            .map_err(ArtworkError::Fetch)?;

        if !content_type.starts_with("image/") {
            return Ok(PromotionOutcome::Refused(RefusalReason::Undecodable));
        }

        cache
            .promote(
                artist_id,
                ArtworkRole::Portrait,
                source,
                reference,
                FetchedImage {
                    content_type,
                    bytes,
                },
            )
            .await
    }

    pub async fn load(
        &self,
        target: CoverArtTarget,
        if_none_match: Option<&str>,
        size: Option<u32>,
    ) -> Result<ArtworkOutcome, ArtworkError> {
        let Some(resolved) = self.resolve_sized(target, size).await? else {
            return Ok(ArtworkOutcome::Missing);
        };

        match resolved {
            Resolved::Remote(reference) => self.fetch(&reference, if_none_match, size).await,
            Resolved::Ready {
                bytes,
                content_type,
                etag,
            } => Ok(settled(content_type, bytes, etag, if_none_match, None)),
            Resolved::Fallback {
                label,
                bytes,
                content_type,
                etag,
            } => Ok(settled(
                content_type,
                bytes,
                etag,
                if_none_match,
                Some(&label),
            )),
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

    #[cfg(all(test, feature = "postgres-integration"))]
    async fn resolve(&self, target: CoverArtTarget) -> Result<Option<Resolved>, ArtworkError> {
        self.resolve_sized(target, None).await
    }

    async fn resolve_sized(
        &self,
        target: CoverArtTarget,
        size: Option<u32>,
    ) -> Result<Option<Resolved>, ArtworkError> {
        match target {
            CoverArtTarget::Track(track_id) => {
                let Some(library) = &self.library else {
                    return Ok(None);
                };
                let Some(sources) = &self.sources else {
                    return Ok(None);
                };

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
                let Some(library) = &self.library else {
                    return Ok(None);
                };
                let Some(sources) = &self.sources else {
                    return Ok(None);
                };

                self.album_artwork(library, sources, &album_id).await
            }
            CoverArtTarget::Artist(artist_id) => self.artist_resolution(&artist_id, size).await,
        }
    }

    async fn artist_resolution(
        &self,
        artist_id: &ArtistId,
        size: Option<u32>,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let (Some(library), Some(sources)) = (&self.library, &self.sources) else {
            return Ok(None);
        };

        let Some(cache) = &self.cache else {
            return self.artist_artwork(library, sources, artist_id).await;
        };

        if let Some(ready) =
            cached_or_absent(cache.ready(artist_id, ArtworkRole::Portrait, size).await)
        {
            return Ok(Some(ready_resolved(ready)));
        }
        if let Some(fallback) =
            cached_or_absent(cache.ready(artist_id, ArtworkRole::Wide, size).await)
        {
            return Ok(Some(fallback_resolved(fallback)));
        }

        self.artist_artwork(library, sources, artist_id).await
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

        if let Some(stored) = self.stored_for_entries(library, sources, &entries).await? {
            return Ok(Some(stored));
        }

        self.stored_for_album(library, sources, album_id).await
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
                move || local_picture(&path)
            })
            .await
            .map_err(|_| ArtworkError::Unavailable)?;

            match read {
                Ok(Some((picture_path, picture))) => {
                    return Ok(Some(Resolved::Local {
                        path: picture_path,
                        picture,
                    }));
                }
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

    async fn stored_for_album(
        &self,
        _library: &Arc<dyn LibraryRead>,
        sources: &Arc<dyn SourceLookup>,
        album_id: &AlbumId,
    ) -> Result<Option<Resolved>, ArtworkError> {
        let Some((_, artwork)) = sources
            .find_album_artwork(album_id)
            .await
            .map_err(ArtworkError::Repository)?
        else {
            return Ok(None);
        };

        Ok(Some(Resolved::Remote(artwork.reference().to_owned())))
    }
}

fn local_picture(path: &Path) -> Result<Option<(std::path::PathBuf, EmbeddedPicture)>, TagError> {
    let embedded = match read_cover_art(path) {
        Ok(Some(picture)) => vec![(path.to_path_buf(), picture)],
        Ok(None) => Vec::new(),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "embedded cover art could not be read");
            Vec::new()
        }
    };

    Ok(select_local_picture(embedded, || {
        path.parent().and_then(folder_picture)
    }))
}

fn select_local_picture(
    embedded: impl IntoIterator<Item = (std::path::PathBuf, EmbeddedPicture)>,
    folder_lookup: impl FnOnce() -> Option<(std::path::PathBuf, EmbeddedPicture)>,
) -> Option<(std::path::PathBuf, EmbeddedPicture)> {
    embedded
        .into_iter()
        .find(|(_, picture)| picture.bytes.len() as u64 <= LOCAL_MAX_PICTURE_BYTES)
        .or_else(folder_lookup)
}

fn folder_picture(directory: &Path) -> Option<(std::path::PathBuf, EmbeddedPicture)> {
    for name in ["cover", "folder", "front"] {
        for extension in ["jpg", "jpeg", "png", "webp"] {
            let candidate = directory.join(format!("{name}.{extension}"));
            let Some(candidate) = existing_case_insensitive(&candidate, directory, name, extension)
            else {
                continue;
            };
            let metadata = match std::fs::metadata(&candidate) {
                Ok(metadata) => metadata,
                Err(error) => {
                    tracing::debug!(%error, path = %candidate.display(), "folder artwork metadata could not be read");
                    continue;
                }
            };
            if metadata.len() > LOCAL_MAX_PICTURE_BYTES {
                tracing::debug!(path = %candidate.display(), bytes = metadata.len(), "folder artwork exceeds the local picture limit");
                continue;
            }
            let bytes = match std::fs::read(&candidate) {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::debug!(%error, path = %candidate.display(), "folder artwork could not be read");
                    continue;
                }
            };
            let Some(content_type) = sniff_picture(&bytes) else {
                tracing::debug!(path = %candidate.display(), "folder artwork has an unknown picture type");
                continue;
            };
            return Some((
                candidate,
                EmbeddedPicture {
                    content_type,
                    bytes,
                },
            ));
        }
    }

    None
}

fn existing_case_insensitive(
    expected: &Path,
    directory: &Path,
    name: &str,
    extension: &str,
) -> Option<std::path::PathBuf> {
    if expected.is_file() {
        return Some(expected.to_path_buf());
    }
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .find_map(|entry| {
            let file_name = entry.file_name();
            let file_name = file_name.to_str()?;
            let (stem, suffix) = file_name.rsplit_once('.')?;
            (stem.eq_ignore_ascii_case(name) && suffix.eq_ignore_ascii_case(extension))
                .then(|| entry.path())
        })
}

fn cached_or_absent(
    ready: Result<Option<super::ReadyAsset>, ArtworkError>,
) -> Option<super::ReadyAsset> {
    match ready {
        Ok(ready) => ready,
        Err(error) => {
            tracing::debug!(%error, "a ready artwork asset was not served");
            None
        }
    }
}

fn ready_resolved(ready: super::ReadyAsset) -> Resolved {
    Resolved::Ready {
        bytes: ready.bytes,
        content_type: ready.descriptor.content_type,
        etag: ready.etag,
    }
}

fn fallback_resolved(ready: super::ReadyAsset) -> Resolved {
    Resolved::Fallback {
        label: ready.descriptor.role.as_str().to_owned(),
        bytes: ready.bytes,
        content_type: ready.descriptor.content_type,
        etag: ready.etag,
    }
}

fn settled(
    content_type: String,
    bytes: Bytes,
    etag: String,
    if_none_match: Option<&str>,
    label: Option<&str>,
) -> ArtworkOutcome {
    if fresh(if_none_match, &etag) {
        return ArtworkOutcome::NotModified(etag);
    }

    match label {
        Some(label) => ArtworkOutcome::Found(Artwork::labelled(
            content_type,
            bytes,
            etag,
            label.to_owned(),
        )),
        None => ArtworkOutcome::Found(Artwork::new(content_type, bytes, etag)),
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

#[cfg(test)]
mod local_picture_tests {
    use super::*;

    #[test]
    fn an_over_limit_embedded_picture_falls_back_to_the_folder_lookup() {
        let audio_path = std::path::PathBuf::from("album/track.flac");
        let folder_path = std::path::PathBuf::from("album/cover.jpg");
        let folder_picture = EmbeddedPicture {
            content_type: "image/jpeg".to_owned(),
            bytes: vec![0xFF, 0xD8, 0xFF],
        };
        let selected = select_local_picture(
            [(
                audio_path,
                EmbeddedPicture {
                    content_type: "image/jpeg".to_owned(),
                    bytes: vec![0; LOCAL_MAX_PICTURE_BYTES as usize + 1],
                },
            )],
            || Some((folder_path.clone(), folder_picture.clone())),
        );

        assert_eq!(selected, Some((folder_path, folder_picture)));
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "service_ready_tests.rs"]
mod ready_tests;
