use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering},
    },
};

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::tracks::TrackRepository;

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

struct LastScan {
    at: chrono::DateTime<chrono::Utc>,
    report: ScanReport,
}

pub struct ScanService {
    repository: Option<Arc<dyn TrackRepository>>,
    roots: Vec<PathBuf>,
    scanning: AtomicBool,
    count: AtomicU32,
    last: Mutex<Option<LastScan>>,
    request: AtomicU8,
    wake: Notify,
}

impl ScanService {
    pub fn new(repository: Arc<dyn TrackRepository>, roots: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            repository: Some(repository),
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
            repository: None,
            roots: Vec::new(),
            scanning: AtomicBool::new(false),
            count: AtomicU32::new(0),
            last: Mutex::new(None),
            request: AtomicU8::new(REQUEST_NONE),
            wake: Notify::new(),
        })
    }

    pub fn is_configured(&self) -> bool {
        self.repository.is_some() && !self.roots.is_empty()
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

    pub async fn run_once(&self, full: bool) -> Result<ScanReport, ScanError> {
        let Some(repository) = &self.repository else {
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
        let result = scan_with(repository, &self.roots, mode, &self.count).await;
        self.scanning.store(false, Ordering::SeqCst);

        if let Ok(report) = &result {
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
    use crate::tracks::InMemoryTrackRepository;

    fn service() -> Arc<ScanService> {
        ScanService::new(
            Arc::new(InMemoryTrackRepository::default()),
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
