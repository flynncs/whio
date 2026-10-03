use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::features::music_library::{
    domain::{ArtistId, SourceIdentity},
    storage::contracts::ArtworkStore,
};

use super::{
    ArtworkError,
    domain::{
        ARTWORK_DERIVED_VARIANTS, ArtworkAssetDescriptor, ArtworkRole, ArtworkVariant,
        FetchedImage, PromotionOutcome, RefusalReason,
    },
    remote, thumbnail,
};

#[derive(Debug, Clone)]
pub struct ReadyAsset {
    pub descriptor: ArtworkAssetDescriptor,
    pub bytes: Bytes,
    pub etag: String,
}

pub struct ArtworkCache {
    store: Arc<dyn ArtworkStore>,
    root: Option<PathBuf>,
    budget_bytes: u64,
}

struct PlannedVariant {
    variant: ArtworkVariant,
    content_type: String,
    content_digest: String,
    width: u32,
    height: u32,
    bytes: Bytes,
}

impl ArtworkCache {
    pub fn new(store: Arc<dyn ArtworkStore>, root: Option<PathBuf>, budget_bytes: u64) -> Self {
        Self {
            store,
            root,
            budget_bytes,
        }
    }

    pub async fn ready(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
        requested: Option<u32>,
    ) -> Result<Option<ReadyAsset>, ArtworkError> {
        let descriptors = self
            .store
            .find_ready(subject, role)
            .await
            .map_err(ArtworkError::Repository)?;
        let Some(descriptor) = select_ready(&descriptors, requested) else {
            return Ok(None);
        };

        let bytes = self.read_ready(&descriptor).await?;
        let etag = ready_etag(&descriptor.content_digest, requested);

        self.store
            .touch(descriptor.id, Utc::now())
            .await
            .map_err(ArtworkError::Repository)?;

        Ok(Some(ReadyAsset {
            descriptor,
            bytes,
            etag,
        }))
    }

    pub async fn promote(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
        source: &SourceIdentity,
        _reference: &str,
        fetched: FetchedImage,
    ) -> Result<PromotionOutcome, ArtworkError> {
        let Some(root) = self.root.as_deref() else {
            return Ok(PromotionOutcome::Refused(RefusalReason::NoCacheDir));
        };

        if fetched.bytes.len() as u64 > remote::MAX_ARTWORK_BYTES {
            return Ok(PromotionOutcome::Refused(RefusalReason::TooManyBytes));
        }

        if !fetched.content_type.starts_with("image/") {
            return Ok(PromotionOutcome::Refused(RefusalReason::Undecodable));
        }

        let Ok(image) = image::load_from_memory(&fetched.bytes) else {
            return Ok(PromotionOutcome::Refused(RefusalReason::Undecodable));
        };

        if u64::from(image.width()) * u64::from(image.height()) > thumbnail::MAX_DECODED_PIXELS {
            return Ok(PromotionOutcome::Refused(RefusalReason::TooManyPixels));
        }

        let existing = self
            .store
            .find_ready(subject, role)
            .await
            .map_err(ArtworkError::Repository)?;
        let provider_id = source.provider_id().as_str();
        let external_id = source.external_id();
        let original_digest = digest(&fetched.bytes);

        if existing.iter().any(|descriptor| {
            descriptor.variant == ArtworkVariant::Original
                && descriptor.source_provider_id == provider_id
                && descriptor.source_external_id == external_id
                && descriptor.content_digest == original_digest
        }) {
            return Ok(PromotionOutcome::AlreadyReady);
        }

        let revision = existing
            .iter()
            .map(|descriptor| descriptor.revision)
            .max()
            .unwrap_or(0)
            + 1;

        let mut planned = vec![PlannedVariant {
            variant: ArtworkVariant::Original,
            content_type: fetched.content_type.clone(),
            content_digest: original_digest,
            width: image.width(),
            height: image.height(),
            bytes: fetched.bytes,
        }];

        for variant in ARTWORK_DERIVED_VARIANTS {
            let Some(size) = variant.size() else {
                continue;
            };
            if let Some((bytes, width, height)) = thumbnail::fitted(&image, size) {
                planned.push(PlannedVariant {
                    variant,
                    content_type: "image/jpeg".to_owned(),
                    content_digest: digest(&bytes),
                    width,
                    height,
                    bytes,
                });
            }
        }

        let set = Uuid::now_v7();
        let mut files: Vec<(ArtworkAssetDescriptor, Bytes)> = Vec::with_capacity(planned.len());
        for item in planned {
            let file_name = artwork_file_name(set, item.variant, &item.content_type);
            files.push((
                ArtworkAssetDescriptor {
                    id: Uuid::now_v7(),
                    subject: subject.clone(),
                    role,
                    source_provider_id: provider_id.to_owned(),
                    source_external_id: external_id.to_owned(),
                    revision,
                    variant: item.variant,
                    content_digest: item.content_digest,
                    content_type: item.content_type,
                    width: item.width,
                    height: item.height,
                    byte_size: item.bytes.len() as i64,
                    file_name,
                },
                item.bytes,
            ));
        }

        let mut written: Vec<PathBuf> = Vec::with_capacity(files.len());
        for (descriptor, bytes) in &files {
            if let Err(error) =
                stage_variant(root, &descriptor.file_name, bytes, &mut written).await
            {
                tracing::debug!(%error, path = %root.display(), "artwork variant was not staged");
                discard(&written).await;
                return Err(ArtworkError::Unavailable);
            }
        }

        let descriptors = files
            .iter()
            .map(|(descriptor, _)| descriptor.clone())
            .collect::<Vec<_>>();

        if let Err(error) = self.store.record_assets(&descriptors).await {
            discard(&written).await;
            return Err(ArtworkError::Repository(error));
        }

        let recorded = self
            .store
            .find_ready(subject, role)
            .await
            .map_err(ArtworkError::Repository)?;
        for (descriptor, _) in &files {
            if recorded.iter().any(|row| row.id == descriptor.id) {
                continue;
            }
            let path = root.join(&descriptor.file_name);
            if let Err(error) = tokio::fs::remove_file(&path).await {
                tracing::debug!(%error, path = %path.display(), "a superseded artwork variant was not removed");
            }
        }

        let protected = descriptors
            .iter()
            .map(|descriptor| descriptor.id)
            .collect::<Vec<_>>();
        self.enforce_budget(root, &protected).await?;

        Ok(PromotionOutcome::Promoted { revision })
    }

    pub async fn read_ready(
        &self,
        descriptor: &ArtworkAssetDescriptor,
    ) -> Result<Bytes, ArtworkError> {
        let Some(root) = self.root.as_deref() else {
            return Err(ArtworkError::Unavailable);
        };

        let path = root.join(&descriptor.file_name);
        tokio::fs::read(&path)
            .await
            .map(Bytes::from)
            .map_err(|_| ArtworkError::Unavailable)
    }

    async fn enforce_budget(&self, root: &Path, protected: &[Uuid]) -> Result<(), ArtworkError> {
        let descriptors = self
            .store
            .lru_descriptors(None, u32::MAX)
            .await
            .map_err(ArtworkError::Repository)?;
        let mut total: u64 = descriptors
            .iter()
            .map(|descriptor| u64::try_from(descriptor.byte_size).unwrap_or(0))
            .sum();

        if total <= self.budget_bytes {
            return Ok(());
        }

        for descriptor in descriptors {
            if total <= self.budget_bytes {
                break;
            }
            if protected.contains(&descriptor.id) {
                continue;
            }

            let deleted = self
                .store
                .delete_asset(descriptor.id)
                .await
                .map_err(ArtworkError::Repository)?;
            if !deleted {
                continue;
            }

            total = total.saturating_sub(u64::try_from(descriptor.byte_size).unwrap_or(0));
            let path = root.join(&descriptor.file_name);
            if let Err(error) = tokio::fs::remove_file(&path).await {
                tracing::debug!(%error, path = %path.display(), "an evicted artwork file was already absent");
            }
        }

        Ok(())
    }
}

fn select_ready(
    descriptors: &[ArtworkAssetDescriptor],
    requested: Option<u32>,
) -> Option<ArtworkAssetDescriptor> {
    let revision = descriptors
        .iter()
        .map(|descriptor| descriptor.revision)
        .max()?;

    let chosen = match requested.map(ArtworkVariant::from_size) {
        None | Some(ArtworkVariant::Original) => None,
        Some(preferred) => {
            let size = preferred.size()?;
            descriptors
                .iter()
                .filter(|descriptor| descriptor.revision == revision)
                .filter(|descriptor| {
                    descriptor
                        .variant
                        .size()
                        .is_some_and(|variant_size| variant_size >= size)
                })
                .min_by_key(|descriptor| descriptor.variant.size())
        }
    };

    chosen
        .or_else(|| {
            descriptors
                .iter()
                .filter(|descriptor| descriptor.revision == revision)
                .max_by_key(|descriptor| descriptor.variant.size().unwrap_or(u32::MAX))
        })
        .cloned()
}

fn ready_etag(content_digest: &str, requested: Option<u32>) -> String {
    match requested {
        Some(size) => format!("\"{content_digest}-w{size}\""),
        None => format!("\"{content_digest}\""),
    }
}

fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);

    hex::encode(hasher.finalize())
}

fn artwork_file_name(set: Uuid, variant: ArtworkVariant, content_type: &str) -> String {
    format!(
        "{set}-{}.{}",
        variant.as_str(),
        image_extension(content_type)
    )
}

fn image_extension(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or_default().trim() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        _ => "bin",
    }
}

async fn stage_variant(
    root: &Path,
    file_name: &str,
    bytes: &[u8],
    written: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    if tokio::fs::metadata(root).await?.permissions().readonly() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "artwork cache directory is read-only",
        ));
    }
    let staged = root.join(format!(".{file_name}.part"));
    let target = root.join(file_name);

    if let Err(error) = tokio::fs::write(&staged, bytes).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    if let Err(error) = tokio::fs::rename(&staged, &target).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }

    written.push(target);
    Ok(())
}

async fn discard(paths: &[PathBuf]) {
    for path in paths {
        if let Err(error) = tokio::fs::remove_file(path).await {
            tracing::debug!(%error, path = %path.display(), "an artwork cache file was not removed");
        }
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "cache_tests.rs"]
mod tests;
