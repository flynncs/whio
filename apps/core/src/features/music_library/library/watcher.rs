use std::{path::PathBuf, sync::Arc, time::Duration};

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};

use super::{scan_service::ScanService, scanner::ScanMode};

pub const DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("media root could not be watched")]
    Unavailable,
}

pub struct MediaWatcher {
    _debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
}

pub fn watch(roots: &[PathBuf], service: Arc<ScanService>) -> Result<MediaWatcher, WatchError> {
    let mut debouncer =
        new_debouncer(
            DEBOUNCE,
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) if !events.is_empty() => service.trigger(ScanMode::Incremental),
                Ok(_) => {}
                Err(errors) => {
                    tracing::warn!(count = errors.len(), "media watcher reported errors");
                    service.trigger(ScanMode::Incremental);
                }
            },
        )
        .map_err(|_| WatchError::Unavailable)?;

    for root in roots {
        debouncer
            .watch(root, RecursiveMode::Recursive)
            .map_err(|_| WatchError::Unavailable)?;
    }

    Ok(MediaWatcher {
        _debouncer: debouncer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_media_root_can_be_watched_and_a_missing_one_is_refused() {
        let root = std::env::temp_dir().join(format!("whio-watch-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let service = ScanService::disabled();

        let watcher = watch(std::slice::from_ref(&root), service);
        assert!(watcher.is_ok());
        drop(watcher);

        let missing = std::env::temp_dir().join(format!("whio-missing-{}", uuid::Uuid::now_v7()));
        let service = ScanService::disabled();
        assert!(watch(std::slice::from_ref(&missing), service).is_err());

        std::fs::remove_dir_all(&root).unwrap();
    }
}
