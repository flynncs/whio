use std::{sync::Arc, time::Instant};

use tokio::{
    fs::{File, OpenOptions},
    sync::watch,
};
use uuid::Uuid;

use crate::{
    features::playback::DeliveryError,
    features::playback::delivery::{
        TransferStats,
        spool_read::SpoolReader,
        spool_status::{SpoolFailure, SpoolStatus, download_to_file},
    },
    features::playback::media::MediaBody,
};

pub struct ActiveSpool {
    id: Uuid,
    path: std::path::PathBuf,
    updates: tokio::sync::watch::Sender<SpoolStatus>,
    _consumer: watch::Receiver<SpoolStatus>,
    task: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    reservation: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl ActiveSpool {
    pub async fn start(
        directory: &std::path::Path,
        body: MediaBody,
        session: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<std::sync::Arc<Self>, DeliveryError> {
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
                DeliveryError::Spool
            })?;

        let (updates, consumer) = watch::channel(SpoolStatus::Downloading { available: 0 });

        tracing::info!(spool_id = %id, expected_bytes, "playback spool started");
        let task = tokio::spawn(download(body, file, updates.clone(), id));

        Ok(Arc::new(Self {
            id,
            path,
            updates,
            _consumer: consumer,
            task: std::sync::Mutex::new(Some(task)),
            reservation: Some(session),
        }))
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn failed(&self) -> bool {
        matches!(*self.updates.borrow(), SpoolStatus::Failed { .. })
    }

    pub fn failure(&self) -> Option<SpoolFailure> {
        match *self.updates.borrow() {
            SpoolStatus::Failed { failure, .. } => Some(failure),
            _ => None,
        }
    }

    pub async fn reader(
        self: &Arc<Self>,
        offset: u64,
        remaining: u64,
    ) -> Result<SpoolReader, DeliveryError> {
        let file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .await
            .map_err(|error| {
                tracing::error!(error = %error, "failed to open playback spool");
                DeliveryError::Spool
            })?;

        SpoolReader::new(
            file,
            self.updates.subscribe(),
            Arc::clone(self),
            offset,
            remaining,
        )
        .await
    }
}

impl Drop for ActiveSpool {
    fn drop(&mut self) {
        let task = self.task.lock().ok().and_then(|mut task| task.take());
        if let Some(task) = &task {
            task.abort();
        }
        drop(self.reservation.take());
        let path = self.path.clone();
        let id = self.id;
        tokio::spawn(async move {
            if let Some(task) = task {
                let _ = task.await;
            }
            match tokio::fs::remove_file(&path).await {
                Ok(()) => tracing::info!(spool_id = %id, "playback spool removed"),
                Err(error) => {
                    tracing::warn!(%error, spool_id = %id, "playback spool not removed");
                }
            }
        });
    }
}

/// Holds an exclusive process lock for the dedicated spool directory.
/// Startup cleanup cannot remove another running core's files.
pub struct SpoolDirectory {
    _lock: std::fs::File,
}
impl SpoolDirectory {
    pub async fn open(directory: &std::path::Path) -> std::io::Result<Self> {
        tokio::fs::create_dir_all(directory).await?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join(".whio-spool.lock"))?;
        lock.try_lock().map_err(std::io::Error::other)?;
        purge_stale(directory).await;
        Ok(Self { _lock: lock })
    }
}

async fn purge_stale(directory: &std::path::Path) -> usize {
    let mut removed = 0;
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return removed;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("whio-") || !name.ends_with(".part") {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::warn!(%error, path = %entry.path().display(), "stale spool not removed")
            }
        }
    }
    if removed > 0 {
        tracing::info!(removed, path = %directory.display(), "stale playback spools removed");
    }
    removed
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
    let (bytes, failure) = match status {
        SpoolStatus::Complete { length } => (length, None),
        SpoolStatus::Failed { available, failure } => (available, Some(failure)),
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

    match failure {
        None => tracing::info!(
            spool_id = %spool_id,
            bytes,
            elapsed_ms,
            average_mbps = %format_args!("{average_mbps:.2}"),
            "playback spool completed"
        ),
        Some(SpoolFailure::Conversion) => tracing::info!(
            spool_id = %spool_id,
            bytes,
            elapsed_ms,
            average_mbps = %format_args!("{average_mbps:.2}"),
            "playback conversion spool stopped"
        ),
        Some(_) => tracing::warn!(
            spool_id = %spool_id,
            bytes,
            elapsed_ms,
            average_mbps = %format_args!("{average_mbps:.2}"),
            "playback spool failed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::features::playback::media::MediaFetchError;

    fn test_dir(name: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("whio-{name}-{suffix}"));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn pending_body() -> MediaBody {
        MediaBody {
            content_length: Some(10),
            chunks: Box::pin(tokio_stream::pending::<Result<bytes::Bytes, MediaFetchError>>()),
        }
    }

    fn session() -> tokio::sync::OwnedSemaphorePermit {
        let sessions = Arc::new(tokio::sync::Semaphore::new(16));
        sessions.try_acquire_owned().unwrap()
    }

    #[tokio::test]
    async fn purge_removes_only_stale_spools() {
        let dir = test_dir("purge");
        tokio::fs::write(dir.join("whio-abc.part"), b"stale")
            .await
            .unwrap();
        tokio::fs::write(dir.join("whio-def.other"), b"keep")
            .await
            .unwrap();
        tokio::fs::write(dir.join("keep.part"), b"keep")
            .await
            .unwrap();

        assert_eq!(purge_stale(&dir).await, 1);
        assert!(!dir.join("whio-abc.part").exists());
        assert!(dir.join("whio-def.other").exists());
        assert!(dir.join("keep.part").exists());
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[tokio::test]
    async fn dropped_spool_cancels_its_download_and_removes_its_file() {
        let dir = test_dir("drop");
        let spool = ActiveSpool::start(&dir, pending_body(), session())
            .await
            .unwrap();
        let path = spool.path.clone();
        assert!(path.exists());
        drop(spool);

        let mut attempts = 0;
        while path.exists() && attempts < 200 {
            attempts += 1;
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!path.exists());
        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
