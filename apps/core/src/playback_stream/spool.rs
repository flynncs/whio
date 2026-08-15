use std::sync::Arc;

use tokio::{fs::OpenOptions, sync::watch};
use uuid::Uuid;

use crate::{
    media::MediaBody,
    playback_stream::{
        PlaybackStreamError,
        spool_read::SpoolReader,
        spool_status::{SpoolStatus, download_to_file},
    },
};

pub struct ActiveSpool {
    path: std::path::PathBuf,
    updates: tokio::sync::watch::Sender<SpoolStatus>,
}

impl ActiveSpool {
    pub async fn start(
        directory: &std::path::Path,
        body: MediaBody,
    ) -> Result<std::sync::Arc<Self>, PlaybackStreamError> {
        let path = directory.join(format!("whio-{}.part", Uuid::now_v7()));

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to create playback spool");
                PlaybackStreamError::Spool
            })?;

        let (updates, _) = watch::channel(SpoolStatus::Downloading { available: 0 });

        let writer_updates = updates.clone();

        tokio::spawn(async move {
            download_to_file(body, file, writer_updates).await;
        });

        Ok(Arc::new(Self { path, updates }))
    }

    pub async fn reader(
        &self,
        offset: u64,
        remaining: u64,
    ) -> Result<SpoolReader, PlaybackStreamError> {
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to open playback spool");
                PlaybackStreamError::Spool
            })?;

        SpoolReader::new(file, self.updates.subscribe(), offset, remaining).await
    }
}
