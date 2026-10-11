use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::{
    catalogue::{LibraryService, MAX_PAGE_LIMIT, PageLimit, TrackListFilter, TrackOrder},
    domain::{AlbumId, RestoreQueueSnapshot, TrackId},
    queue::RestoreQueueService,
};

#[async_trait]
pub trait NextTracks: Send + Sync {
    async fn next(&self, user: Option<Uuid>, track_id: &TrackId, count: usize) -> Vec<TrackId>;
}

pub struct CatalogueNextTracks {
    queue: Arc<RestoreQueueService>,
    library: Arc<LibraryService>,
}

enum QueueSelection {
    Selected(Vec<TrackId>),
    Absent,
    Failed,
}

impl CatalogueNextTracks {
    pub fn new(queue: Arc<RestoreQueueService>, library: Arc<LibraryService>) -> Self {
        Self { queue, library }
    }

    async fn next_in_queue(
        &self,
        user: Option<Uuid>,
        track_id: &TrackId,
        count: usize,
    ) -> QueueSelection {
        let Some(user) = user else {
            return QueueSelection::Absent;
        };
        match self.queue.read(user).await {
            Ok(Some(snapshot)) => match queue_successors(&snapshot, track_id, count) {
                Some(successors) => QueueSelection::Selected(successors),
                None => QueueSelection::Absent,
            },
            Ok(None) => QueueSelection::Absent,
            Err(error) => {
                tracing::debug!(error = %error, "playback prefetch could not read the restore queue");
                QueueSelection::Failed
            }
        }
    }

    async fn next_in_album(
        &self,
        user: Option<Uuid>,
        track_id: &TrackId,
        count: usize,
    ) -> Vec<TrackId> {
        let Some(album_id) = self.album_id(user, track_id).await else {
            return Vec::new();
        };
        let limit = PageLimit::new(MAX_PAGE_LIMIT).expect("album page limit");
        let mut successors = Vec::new();
        let mut offset = 0u32;
        let mut found = false;
        loop {
            let page = match self
                .library
                .album_track_page(
                    &album_id,
                    offset,
                    limit,
                    TrackOrder::Track,
                    &TrackListFilter::new(),
                    user.as_ref(),
                )
                .await
            {
                Ok(page) => page,
                Err(error) => {
                    tracing::debug!(error = %error, "playback prefetch could not read the album");
                    return Vec::new();
                }
            };

            let items = page.items();
            if found {
                successors.extend(items.iter().map(|view| view.track().id().clone()));
            } else if let Some(index) = items.iter().position(|view| view.track().id() == track_id)
            {
                found = true;
                successors.extend(
                    items[index + 1..]
                        .iter()
                        .map(|view| view.track().id().clone()),
                );
            }

            if found && successors.len() >= count {
                successors.truncate(count);
                return successors;
            }
            offset += items.len() as u32;
            if items.is_empty() || offset >= page.total() {
                return successors;
            }
        }
    }

    async fn album_id(&self, user: Option<Uuid>, track_id: &TrackId) -> Option<AlbumId> {
        match self
            .library
            .track_views(std::slice::from_ref(track_id), user.as_ref())
            .await
        {
            Ok(views) => views
                .into_iter()
                .next()?
                .album()
                .map(|album| album.id().clone()),
            Err(error) => {
                tracing::debug!(error = %error, "playback prefetch could not read the track");
                None
            }
        }
    }
}

#[async_trait]
impl NextTracks for CatalogueNextTracks {
    async fn next(&self, user: Option<Uuid>, track_id: &TrackId, count: usize) -> Vec<TrackId> {
        if count == 0 {
            return Vec::new();
        }
        match self.next_in_queue(user, track_id, count).await {
            QueueSelection::Selected(successors) => successors,
            QueueSelection::Failed => Vec::new(),
            QueueSelection::Absent => self.next_in_album(user, track_id, count).await,
        }
    }
}

fn queue_successors(
    snapshot: &RestoreQueueSnapshot,
    track_id: &TrackId,
    count: usize,
) -> Option<Vec<TrackId>> {
    let entries = snapshot.entries();
    let current = snapshot
        .current_index()
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| {
            entries
                .get(*index)
                .is_some_and(|entry| entry.track_id() == track_id)
        });
    let start = current.or_else(|| {
        entries
            .iter()
            .position(|entry| entry.track_id() == track_id)
    })?;
    Some(
        entries[start + 1..]
            .iter()
            .take(count)
            .map(|entry| entry.track_id().clone())
            .collect(),
    )
}

#[cfg(test)]
pub(crate) mod fixtures {
    use std::{collections::HashMap, sync::Arc};

    use async_trait::async_trait;
    use uuid::Uuid;

    use crate::features::music_library::{
        catalogue::{LibraryService, TrackListFilter, TrackOrder},
        domain::{
            Album, AlbumId, LibraryState, ProviderId, RestoreQueueEntry, RestoreQueueEntryId,
            RestoreQueueSnapshot, RestoreQueueWrite, RestoreQueueWriteOutcome, SourceIdentity,
            SourceScope, Track, TrackId, TrackMetadata, TrackPage, TrackView,
        },
        queue::RestoreQueueService,
        test_support::{
            LibraryRead, RestoreQueueStore, SourceLookup, StateStore, TrackRepositoryError,
        },
    };

    pub(crate) fn youtube(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    pub(crate) fn local() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            "recording-1".to_owned(),
        )
        .unwrap()
    }

    #[derive(Default)]
    pub(crate) struct FakeSources {
        sources: HashMap<TrackId, Vec<SourceIdentity>>,
    }

    impl FakeSources {
        pub(crate) fn new(entries: Vec<(TrackId, Vec<SourceIdentity>)>) -> Self {
            Self {
                sources: entries.into_iter().collect(),
            }
        }
    }

    #[async_trait]
    impl SourceLookup for FakeSources {
        async fn find_sources(
            &self,
            track_id: &TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            Ok(self.sources.get(track_id).cloned().unwrap_or_default())
        }
    }

    pub(crate) struct NoState;

    #[async_trait]
    impl StateStore for NoState {}

    pub(crate) struct FakeQueue {
        snapshot: Option<RestoreQueueSnapshot>,
        failing: bool,
    }

    impl FakeQueue {
        pub(crate) fn empty() -> Self {
            Self {
                snapshot: None,
                failing: false,
            }
        }

        pub(crate) fn failing() -> Self {
            Self {
                snapshot: None,
                failing: true,
            }
        }

        pub(crate) fn with_entries(
            user: Uuid,
            entries: Vec<TrackId>,
            current_index: Option<u32>,
        ) -> Self {
            let entries = entries
                .into_iter()
                .enumerate()
                .map(|(position, track_id)| {
                    RestoreQueueEntry::new(RestoreQueueEntryId::new(), position as u32, track_id)
                })
                .collect();
            Self {
                snapshot: Some(RestoreQueueSnapshot::new(
                    user,
                    user,
                    "listener".to_owned(),
                    current_index,
                    0,
                    1,
                    chrono::Utc::now(),
                    entries,
                )),
                failing: false,
            }
        }
    }

    #[async_trait]
    impl RestoreQueueStore for FakeQueue {
        async fn read_restore_queue(
            &self,
            _user: Uuid,
        ) -> Result<Option<RestoreQueueSnapshot>, TrackRepositoryError> {
            if self.failing {
                return Err(TrackRepositoryError::Unavailable);
            }
            Ok(self.snapshot.clone())
        }

        async fn save_restore_queue(
            &self,
            _write: RestoreQueueWrite,
        ) -> Result<RestoreQueueWriteOutcome, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn clear_restore_queue(&self, _user: Uuid) -> Result<bool, TrackRepositoryError> {
            Ok(false)
        }
    }

    pub(crate) struct FakeLibrary {
        album: Album,
        tracks: Vec<Track>,
    }

    impl FakeLibrary {
        pub(crate) fn new(tracks: Vec<Track>) -> Self {
            Self {
                album: Album::new(
                    AlbumId::new(),
                    "Album".to_owned(),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap(),
                tracks,
            }
        }

        fn views(&self) -> Vec<TrackView> {
            self.tracks
                .iter()
                .map(|track| {
                    TrackView::new(
                        track.clone(),
                        Some(self.album.clone()),
                        None,
                        None,
                        LibraryState::default(),
                        Vec::new(),
                    )
                })
                .collect()
        }
    }

    #[async_trait]
    impl LibraryRead for FakeLibrary {
        async fn find_track(
            &self,
            track_id: &TrackId,
        ) -> Result<Option<Track>, TrackRepositoryError> {
            Ok(self
                .tracks
                .iter()
                .find(|track| track.id() == track_id)
                .cloned())
        }

        async fn page_tracks(
            &self,
            _offset: u32,
            _limit: u32,
            _order: TrackOrder,
            _filter: &TrackListFilter,
            _user: Option<Uuid>,
        ) -> Result<TrackPage, TrackRepositoryError> {
            Ok(TrackPage::new(Vec::new(), 0))
        }

        async fn track_views(
            &self,
            track_ids: &[TrackId],
            _user: Option<Uuid>,
        ) -> Result<Vec<TrackView>, TrackRepositoryError> {
            Ok(self
                .views()
                .into_iter()
                .filter(|view| track_ids.contains(view.track().id()))
                .collect())
        }

        async fn page_album_tracks(
            &self,
            _album_id: &AlbumId,
            offset: u32,
            limit: u32,
            _order: TrackOrder,
            _filter: &TrackListFilter,
            _user: Option<Uuid>,
        ) -> Result<TrackPage, TrackRepositoryError> {
            let views = self.views();
            let total = views.len() as u32;
            let items = views
                .into_iter()
                .skip(offset as usize)
                .take(limit as usize)
                .collect();
            Ok(TrackPage::new(items, total))
        }
    }

    pub(crate) fn track(title: &str) -> Track {
        Track::new(
            TrackId::new(),
            TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000))
                .unwrap(),
        )
    }

    pub(crate) fn library(tracks: Vec<Track>) -> Arc<LibraryService> {
        Arc::new(LibraryService::new(
            Arc::new(FakeLibrary::new(tracks)),
            Arc::new(FakeSources::default()),
            Arc::new(NoState),
        ))
    }

    pub(crate) fn queue(snapshot: FakeQueue) -> Arc<RestoreQueueService> {
        Arc::new(RestoreQueueService::new(Arc::new(snapshot)))
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        CatalogueNextTracks, NextTracks,
        fixtures::{FakeQueue, library, queue, track},
    };
    use crate::features::music_library::domain::{Track, TrackId};

    const USER: Uuid = Uuid::from_u128(1);

    async fn next(
        snapshot: FakeQueue,
        tracks: Vec<Track>,
        track_id: &TrackId,
        count: usize,
    ) -> Vec<TrackId> {
        CatalogueNextTracks::new(queue(snapshot), library(tracks))
            .next(Some(USER), track_id, count)
            .await
    }

    #[tokio::test]
    async fn an_exhausted_queue_selects_no_album_successor() {
        let first = track("One");
        let second = track("Two");
        let successors = next(
            FakeQueue::with_entries(USER, vec![first.id().clone()], Some(0)),
            vec![first.clone(), second],
            first.id(),
            1,
        )
        .await;

        assert!(successors.is_empty());
    }

    #[tokio::test]
    async fn a_duplicate_occurrence_after_the_current_index_wins() {
        let first = track("One");
        let second = track("Two");
        let third = track("Three");
        let successors = next(
            FakeQueue::with_entries(
                USER,
                vec![
                    first.id().clone(),
                    second.id().clone(),
                    first.id().clone(),
                    third.id().clone(),
                ],
                Some(2),
            ),
            Vec::new(),
            first.id(),
            3,
        )
        .await;

        assert_eq!(successors, vec![third.id().clone()]);
    }

    #[tokio::test]
    async fn an_absent_track_falls_back_to_album_order() {
        let first = track("One");
        let second = track("Two");
        let absent = track("Absent");
        let successors = next(
            FakeQueue::with_entries(USER, vec![absent.id().clone()], Some(0)),
            vec![first.clone(), second.clone()],
            first.id(),
            1,
        )
        .await;

        assert_eq!(successors, vec![second.id().clone()]);
    }

    #[tokio::test]
    async fn a_queue_read_error_yields_empty() {
        let first = track("One");
        let second = track("Two");
        let successors = next(
            FakeQueue::failing(),
            vec![first.clone(), second],
            first.id(),
            1,
        )
        .await;

        assert!(successors.is_empty());
    }

    #[tokio::test]
    async fn album_successors_cross_the_page_boundary() {
        let tracks: Vec<_> = (0..700).map(|index| track(&format!("T{index}"))).collect();
        let successors = next(FakeQueue::empty(), tracks.clone(), tracks[499].id(), 3).await;

        assert_eq!(
            successors,
            vec![
                tracks[500].id().clone(),
                tracks[501].id().clone(),
                tracks[502].id().clone()
            ]
        );
    }

    #[tokio::test]
    async fn an_album_track_beyond_the_first_page_has_successors() {
        let tracks: Vec<_> = (0..700).map(|index| track(&format!("T{index}"))).collect();
        let successors = next(FakeQueue::empty(), tracks.clone(), tracks[600].id(), 1).await;

        assert_eq!(successors, vec![tracks[601].id().clone()]);
    }
}
