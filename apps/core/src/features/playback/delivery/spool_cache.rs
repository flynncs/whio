use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use tokio::sync::OnceCell;

use crate::{
    features::playback::{Representation, SourceKey, delivery::ActiveSpool, media::MediaInfo},
    platform::clock::Clock,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct SpoolKey {
    source: SourceKey,
    representation: Representation,
}

impl SpoolKey {
    pub(super) fn original(source: &SourceKey) -> Self {
        Self {
            source: source.clone(),
            representation: Representation::Original,
        }
    }

    pub(super) fn new(source: &SourceKey, representation: Representation) -> Self {
        Self {
            source: source.clone(),
            representation,
        }
    }
}

pub(super) struct Entry {
    spool: Arc<ActiveSpool>,
    info: MediaInfo,
    last_used: Mutex<DateTime<Utc>>,
}

impl Entry {
    fn idle(&self, now: DateTime<Utc>, idle: chrono::Duration) -> bool {
        Arc::strong_count(&self.spool) == 1 && now - *self.last_used.lock() > idle
    }

    fn idle_candidate(&self) -> Option<DateTime<Utc>> {
        (Arc::strong_count(&self.spool) == 1).then(|| *self.last_used.lock())
    }
}

pub(super) struct Cached {
    cell: Arc<OnceCell<Entry>>,
    pub(super) spool: Arc<ActiveSpool>,
    pub(super) info: MediaInfo,
}

impl Cached {
    pub(super) fn failed(&self) -> bool {
        self.spool.failed()
    }
}

pub(super) struct SpoolCache {
    cells: Mutex<HashMap<SpoolKey, Arc<OnceCell<Entry>>>>,
    clock: Clock,
    idle: chrono::Duration,
}

fn idle_duration(idle: Duration) -> chrono::Duration {
    chrono::Duration::from_std(idle).unwrap_or(chrono::Duration::MAX)
}

fn remove_if_current(
    cells: &mut HashMap<SpoolKey, Arc<OnceCell<Entry>>>,
    key: &SpoolKey,
    cell: &Arc<OnceCell<Entry>>,
) {
    if cells
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, cell))
    {
        cells.remove(key);
    }
}

impl SpoolCache {
    pub(super) fn new(clock: Clock, idle: Duration) -> Self {
        Self {
            cells: Mutex::new(HashMap::new()),
            clock,
            idle: idle_duration(idle),
        }
    }

    pub(super) fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub(super) fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = idle_duration(idle);
        self
    }

    pub(super) fn entry(&self, spool: Arc<ActiveSpool>, info: MediaInfo) -> Entry {
        Entry {
            spool,
            info,
            last_used: Mutex::new(self.clock.now()),
        }
    }

    pub(super) fn cell(&self, key: &SpoolKey) -> Arc<OnceCell<Entry>> {
        let mut cells = self.cells.lock();
        let failed = cells
            .get(key)
            .cloned()
            .filter(|cell| cell.get().is_some_and(|entry| entry.spool.failed()));
        if let Some(failed) = failed {
            remove_if_current(&mut cells, key, &failed);
        }
        Arc::clone(
            cells
                .entry(key.clone())
                .or_insert_with(|| Arc::new(OnceCell::new())),
        )
    }

    pub(super) async fn get_or_init<E, F, Fut>(
        &self,
        key: &SpoolKey,
        create: F,
    ) -> Result<Cached, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Entry, E>>,
    {
        let cell = self.cell(key);
        let entry = cell.get_or_try_init(create).await?;
        let spool = Arc::clone(&entry.spool);
        let info = entry.info.clone();
        Ok(Cached { cell, spool, info })
    }

    pub(super) fn get(&self, key: &SpoolKey) -> Option<Cached> {
        let now = self.clock.now();
        let mut cells = self.cells.lock();
        let cell = Arc::clone(cells.get(key)?);
        let entry = cell.get()?;
        if entry.spool.failed() {
            cells.remove(key);
            return None;
        }
        *entry.last_used.lock() = now;
        let spool = Arc::clone(&entry.spool);
        let info = entry.info.clone();
        Some(Cached { cell, spool, info })
    }

    pub(super) fn is_cached(&self, key: &SpoolKey) -> bool {
        self.cells
            .lock()
            .get(key)
            .and_then(|cell| cell.get())
            .is_some_and(|entry| !entry.spool.failed())
    }

    pub(super) fn invalidate(&self, key: &SpoolKey, cached: &Cached) {
        remove_if_current(&mut self.cells.lock(), key, &cached.cell);
    }

    pub(super) fn evict_idle(&self) {
        let now = self.clock.now();
        self.cells
            .lock()
            .retain(|_, cell| !cell.get().is_some_and(|entry| entry.idle(now, self.idle)));
    }

    pub(super) fn evict_one_idle(&self) -> bool {
        let mut cells = self.cells.lock();
        let candidate = cells
            .iter()
            .filter_map(|(key, cell)| {
                cell.get()
                    .and_then(Entry::idle_candidate)
                    .map(|last_used| (key, last_used))
            })
            .min_by_key(|(_, last_used)| *last_used)
            .map(|(key, _)| key.clone());
        match candidate {
            Some(key) => cells.remove(&key).is_some(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::http::StatusCode;
    use tokio::sync::Semaphore;

    use super::*;
    use crate::{
        features::{
            music_library::domain::{ProviderId, SourceIdentity, SourceScope},
            playback::{
                PlaybackSource,
                media::{MediaBody, MediaFetchError},
            },
        },
        platform::clock::Clock,
    };

    fn source(external_id: &str) -> SourceKey {
        SourceKey::new(
            &PlaybackSource::from_identity(
                SourceIdentity::new(
                    ProviderId::new("youtube_music".to_owned()).unwrap(),
                    SourceScope::Global,
                    external_id.to_owned(),
                )
                .unwrap(),
            )
            .unwrap(),
        )
    }

    fn key(external_id: &str) -> SpoolKey {
        SpoolKey::original(&source(external_id))
    }

    fn info() -> MediaInfo {
        MediaInfo {
            content_length: 10,
            supports_ranges: true,
            content_type: None,
        }
    }

    async fn spool() -> Arc<ActiveSpool> {
        let sessions = Arc::new(Semaphore::new(16));
        let permit = sessions.try_acquire_owned().unwrap();
        let body = MediaBody {
            content_length: Some(10),
            chunks: Box::pin(tokio_stream::pending::<Result<bytes::Bytes, MediaFetchError>>()),
        };
        ActiveSpool::start(&std::env::temp_dir(), body, permit)
            .await
            .unwrap()
    }

    fn manual_clock() -> (Clock, Arc<Mutex<chrono::DateTime<Utc>>>) {
        let now = Arc::new(Mutex::new(Utc::now()));
        let handle = Arc::clone(&now);
        (Clock::from_fn(move || *handle.lock()), now)
    }

    async fn failed_spool() -> Arc<ActiveSpool> {
        let sessions = Arc::new(Semaphore::new(16));
        let permit = sessions.try_acquire_owned().unwrap();
        let body = MediaBody {
            content_length: Some(10),
            chunks: Box::pin(tokio_stream::iter(vec![Err(
                MediaFetchError::UnexpectedStatus(StatusCode::BAD_GATEWAY),
            )])),
        };
        let spool = ActiveSpool::start(&std::env::temp_dir(), body, permit)
            .await
            .unwrap();
        let mut reader = spool.reader(0, 1).await.unwrap();
        assert!(reader.next_chunk().await.is_err());
        assert!(spool.failed());
        spool
    }

    async fn insert(cache: &SpoolCache, key: &SpoolKey, spool: Arc<ActiveSpool>) {
        cache
            .get_or_init(key, || async { Ok::<_, ()>(cache.entry(spool, info())) })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn idle_entries_are_evicted_after_the_idle_time() {
        let (clock, now) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(60));
        insert(&cache, &key("one"), spool().await).await;

        *now.lock() += chrono::Duration::seconds(59);
        cache.evict_idle();
        assert!(cache.get(&key("one")).is_some());

        *now.lock() += chrono::Duration::seconds(61);
        cache.evict_idle();
        assert!(cache.get(&key("one")).is_none());
    }

    #[tokio::test]
    async fn an_entry_with_a_reader_is_not_evicted() {
        let (clock, now) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(1));
        insert(&cache, &key("one"), spool().await).await;

        let cached = cache.get(&key("one")).unwrap();
        *now.lock() += chrono::Duration::seconds(3600);
        cache.evict_idle();

        assert!(cache.get(&key("one")).is_some());
        drop(cached);
        *now.lock() += chrono::Duration::seconds(3600);
        cache.evict_idle();
        assert!(cache.get(&key("one")).is_none());
    }

    #[tokio::test]
    async fn is_cached_does_not_refresh_the_last_used_time() {
        let (clock, now) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(60));
        insert(&cache, &key("one"), spool().await).await;

        *now.lock() += chrono::Duration::seconds(59);
        assert!(cache.is_cached(&key("one")));

        *now.lock() += chrono::Duration::seconds(2);
        cache.evict_idle();
        assert!(!cache.is_cached(&key("one")));
    }

    #[tokio::test]
    async fn an_uninitialized_cell_is_never_evicted() {
        let (clock, _) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(1));
        let cell = cache.cell(&key("one"));

        cache.evict_idle();
        assert!(!cache.evict_one_idle());
        assert!(Arc::ptr_eq(&cell, &cache.cell(&key("one"))));
        assert!(!cache.is_cached(&key("one")));
    }

    #[tokio::test]
    async fn invalidation_targets_the_observed_cell() {
        let (clock, _) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(60));
        insert(&cache, &key("one"), spool().await).await;
        let older = cache.get(&key("one")).unwrap();

        cache.invalidate(&key("one"), &older);
        assert!(cache.get(&key("one")).is_none());

        let newer_spool = spool().await;
        insert(&cache, &key("one"), Arc::clone(&newer_spool)).await;
        cache.invalidate(&key("one"), &older);

        let (held, _) = (cache.get(&key("one")).unwrap().spool, ());
        assert!(Arc::ptr_eq(&held, &newer_spool));
    }

    #[tokio::test]
    async fn a_failed_cell_is_replaced_before_initialization() {
        let (clock, _) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(60));
        let failed = failed_spool().await;
        cache
            .get_or_init(&key("one"), || async {
                Ok::<_, ()>(cache.entry(Arc::clone(&failed), info()))
            })
            .await
            .unwrap();
        assert!(failed.failed());
        assert!(!cache.is_cached(&key("one")));

        let replacement = spool().await;
        let calls = AtomicUsize::new(0);
        let cached = cache
            .get_or_init(&key("one"), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(cache.entry(Arc::clone(&replacement), info()))
            })
            .await
            .unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&cached.spool, &replacement));
        assert!(cache.is_cached(&key("one")));
    }

    #[tokio::test]
    async fn evict_one_idle_removes_the_least_recently_used_entry() {
        let (clock, now) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(1));
        insert(&cache, &key("one"), spool().await).await;
        *now.lock() += chrono::Duration::seconds(1);
        insert(&cache, &key("two"), spool().await).await;

        assert!(cache.evict_one_idle());
        assert!(cache.get(&key("one")).is_none());
        assert!(cache.get(&key("two")).is_some());

        assert!(cache.evict_one_idle());
        assert!(!cache.evict_one_idle());
    }

    #[tokio::test]
    async fn evict_one_idle_skips_entries_with_a_reader() {
        let (clock, _) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(1));
        insert(&cache, &key("one"), spool().await).await;
        let cached = cache.get(&key("one")).unwrap();

        assert!(!cache.evict_one_idle());
        drop(cached);
        assert!(cache.evict_one_idle());
    }

    #[tokio::test]
    async fn representations_of_one_source_have_distinct_cells() {
        let (clock, _) = manual_clock();
        let cache = SpoolCache::new(clock, Duration::from_secs(60));
        let original = SpoolKey::original(&source("one"));
        let converted = SpoolKey::new(
            &source("one"),
            Representation::Aac(crate::features::playback::AacMode::Copy),
        );

        insert(&cache, &original, spool().await).await;
        insert(&cache, &converted, spool().await).await;
        assert!(cache.is_cached(&original));
        assert!(cache.is_cached(&converted));
        assert!(!Arc::ptr_eq(
            &cache.cell(&original),
            &cache.cell(&converted)
        ));
    }
}
