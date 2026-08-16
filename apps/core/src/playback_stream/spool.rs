use std::{sync::Arc, time::Instant};

use tokio::{
    fs::{File, OpenOptions},
    sync::watch,
};
use uuid::Uuid;

use crate::{
    media::MediaBody,
    playback_stream::{
        PlaybackStreamError, TransferStats,
        spool_read::SpoolReader,
        spool_status::{SpoolStatus, download_to_file},
    },
};

pub struct ActiveSpool {
    id: Uuid,
    path: std::path::PathBuf,
    updates: tokio::sync::watch::Sender<SpoolStatus>,
}

impl ActiveSpool {
    pub async fn start(
        directory: &std::path::Path,
        body: MediaBody,
    ) -> Result<std::sync::Arc<Self>, PlaybackStreamError> {
        let id = Uuid::now_v7();
        let path = directory.join(format!("whio-{id}.part"));
        let expected_bytes = body.content_length;

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

        tracing::info!(spool_id = %id, expected_bytes, "playback spool started");
        tokio::spawn(download(body, file, updates.clone(), id));

        Ok(Arc::new(Self { id, path, updates }))
    }

    pub fn id(&self) -> Uuid {
        self.id
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

async fn download(
    body: MediaBody,
    file: File,
    updates: watch::Sender<SpoolStatus>,
    spool_id: Uuid,
) {
    let started_at = Instant::now();
    download_to_file(body, file, updates.clone()).await;
    log_download_finished(spool_id, updates.borrow().clone(), started_at);
}

fn log_download_finished(spool_id: Uuid, status: SpoolStatus, started_at: Instant) {
    let (bytes, failed) = match status {
        SpoolStatus::Complete { length } => (length, false),
        SpoolStatus::Failed { available } => (available, true),
        SpoolStatus::Downloading { available } => {
            tracing::warn!(
                spool_id = %spool_id,
                bytes = available,
                "playback spool stopped without a terminal status"
            );
            return;
        }
    };

    let stats = TransferStats::since(bytes, started_at);
    let elapsed_ms = stats.elapsed_ms();
    let average_mbps = stats.average_mbps();

    if failed {
        tracing::warn!(
            spool_id = %spool_id,
            bytes,
            elapsed_ms,
            average_mbps = %format_args!("{average_mbps:.2}"),
            "playback spool failed"
        );
    } else {
        tracing::info!(
            spool_id = %spool_id,
            bytes,
            elapsed_ms,
            average_mbps = %format_args!("{average_mbps:.2}"),
            "playback spool completed"
        );
    }
}
