use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::features::music_library::storage::contracts::{CatalogueWrite, SourceLookup};

use super::scanner::{ScanError, ScanMode, ScanReport, scan_with};

const REQUEST_NONE: u8 = 0;
const REQUEST_INCREMENTAL: u8 = 1;
const REQUEST_FULL: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanStatus {
    pub scanning: bool,
    pub count: u32,
    pub last_scan: Option<chrono::DateTime<chrono::Utc>>,
    pub last_report: Option<ScanReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanRequestError {
    Forbidden,
}

struct LastScan {
    at: chrono::DateTime<chrono::Utc>,
    report: ScanReport,
}

pub struct ScanService {
    catalogue: Option<Arc<dyn CatalogueWrite>>,
    sources: Option<Arc<dyn SourceLookup>>,
    roots: Vec<PathBuf>,
    scanning: AtomicBool,
    count: AtomicU32,
    last: Mutex<Option<LastScan>>,
    request: AtomicU8,
    wake: Notify,
}

impl ScanService {
    pub fn new(
        catalogue: Arc<dyn CatalogueWrite>,
        sources: Arc<dyn SourceLookup>,
        roots: Vec<PathBuf>,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalogue: Some(catalogue),
            sources: Some(sources),
            roots,
            scanning: AtomicBool::new(false),
            count: AtomicU32::new(0),
            last: Mutex::new(None),
            request: AtomicU8::new(REQUEST_NONE),
            wake: Notify::new(),
        })
    }

    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            catalogue: None,
            sources: None,
            roots: Vec::new(),
            scanning: AtomicBool::new(false),
            count: AtomicU32::new(0),
            last: Mutex::new(None),
            request: AtomicU8::new(REQUEST_NONE),
            wake: Notify::new(),
        })
    }

    pub fn is_configured(&self) -> bool {
        self.catalogue.is_some() && !self.roots.is_empty()
    }

    pub fn status(&self) -> ScanStatus {
        let last = self.last.lock();

        ScanStatus {
            scanning: self.scanning.load(Ordering::SeqCst),
            count: self.count.load(Ordering::SeqCst),
            last_scan: last.as_ref().map(|last| last.at),
            last_report: last.as_ref().map(|last| last.report.clone()),
        }
    }

    pub fn trigger(&self, mode: ScanMode) {
        let request = match mode {
            ScanMode::Full => REQUEST_FULL,
            ScanMode::Incremental => REQUEST_INCREMENTAL,
        };
        self.request.fetch_max(request, Ordering::SeqCst);
        self.wake.notify_one();
    }

    pub fn request(
        &self,
        principal: &crate::features::identity::domain::Principal,
        mode: ScanMode,
    ) -> Result<ScanStatus, ScanRequestError> {
        if !principal.is_admin() {
            return Err(ScanRequestError::Forbidden);
        }

        self.trigger(mode);
        Ok(self.status())
    }

    pub fn start(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move { service.worker().await });
    }

    async fn worker(self: Arc<Self>) {
        loop {
            let request = self.request.swap(REQUEST_NONE, Ordering::SeqCst);

            if request != REQUEST_NONE {
                if let Err(error) = self.run_once(request == REQUEST_FULL).await {
                    tracing::warn!(error = ?error, "library scan failed");
                }
                continue;
            }

            self.wake.notified().await;
        }
    }

    #[tracing::instrument(skip_all, fields(mode = if full { "full" } else { "incremental" }))]
    pub async fn run_once(&self, full: bool) -> Result<ScanReport, ScanError> {
        let (Some(catalogue), Some(sources)) = (&self.catalogue, &self.sources) else {
            return Err(ScanError::Unreadable);
        };
        if self.roots.is_empty() {
            return Err(ScanError::Unreadable);
        }

        self.scanning.store(true, Ordering::SeqCst);
        self.count.store(0, Ordering::SeqCst);
        let mode = if full {
            ScanMode::Full
        } else {
            ScanMode::Incremental
        };
        let result = scan_with(catalogue, sources, &self.roots, mode, &self.count).await;
        self.scanning.store(false, Ordering::SeqCst);

        if let Ok(report) = &result {
            tracing::info!(
                mode = ?mode,
                indexed = report.indexed,
                unchanged = report.unchanged,
                untagged = report.untagged,
                failed = report.failed,
                missing = report.missing,
                empty = report.empty,
                unwalked = report.unwalked,
                "library scan completed"
            );
            *self.last.lock() = Some(LastScan {
                at: chrono::Utc::now(),
                report: report.clone(),
            });
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::features::music_library::{
        domain::{AlbumId, AlbumTrack, ArtworkRef, SourceIdentity, Track, TrackId, TrackMetadata},
        storage::contracts::{CatalogueWrite, SourceLookup, TrackRepositoryError},
    };

    struct NeverScan;

    #[async_trait::async_trait]
    impl CatalogueWrite for NeverScan {
        async fn get_or_create(
            &self,
            _source: SourceIdentity,
            _metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            panic!("status checks must not write tracks")
        }

        async fn get_or_create_provider(
            &self,
            _source: SourceIdentity,
            _metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            panic!("status checks must not write tracks")
        }

        async fn add_album_track(
            &self,
            _album_id: &AlbumId,
            _track_id: &TrackId,
            _membership: &AlbumTrack,
        ) -> Result<(), TrackRepositoryError> {
            panic!("status checks must not write tracks")
        }
    }

    #[async_trait::async_trait]
    impl SourceLookup for NeverScan {
        async fn find_sources(
            &self,
            _track: &TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            panic!("status checks must not read sources")
        }

        async fn set_album_source_artwork(
            &self,
            _source: &SourceIdentity,
            _artwork: &ArtworkRef,
        ) -> Result<(), TrackRepositoryError> {
            panic!("status checks must not write artwork")
        }

        async fn find_album_artwork(
            &self,
            _album_id: &AlbumId,
        ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
            panic!("status checks must not read artwork")
        }
    }

    fn service() -> Arc<ScanService> {
        let store = Arc::new(NeverScan);
        ScanService::new(
            Arc::clone(&store) as Arc<dyn CatalogueWrite>,
            Arc::clone(&store) as Arc<dyn SourceLookup>,
            vec![std::env::temp_dir()],
        )
    }

    #[tokio::test]
    async fn an_idle_service_reports_no_scan() {
        let status = service().status();

        assert!(!status.scanning);
        assert_eq!(status.count, 0);
        assert!(status.last_scan.is_none());
    }

    #[tokio::test]
    async fn disabled_service_runs_nothing() {
        let service = ScanService::disabled();

        assert!(service.run_once(false).await.is_err());
        assert!(!service.status().scanning);
    }

    #[tokio::test]
    async fn a_trigger_is_coalesced_and_requests_full_when_asked() {
        let service = service();

        service.trigger(ScanMode::Incremental);
        service.trigger(ScanMode::Full);

        assert_eq!(
            service.request.swap(REQUEST_NONE, Ordering::SeqCst),
            REQUEST_FULL
        );
    }
}
