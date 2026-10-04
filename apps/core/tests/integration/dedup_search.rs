use sqlx::PgPool;
use std::{path::PathBuf, sync::Arc, time::Duration};
use uuid::Uuid;
use whio_core::features::music_library::test_support::PostgresTrackRepository;
use whio_core::{
    features::music_library::catalogue::{
        CatalogueSearch, DedupMode, SearchLimit, SearchOffset, SearchQuery, SearchRequest,
        SearchService, SearchWindow,
    },
    features::music_library::domain::{
        AlbumMetadata, AlbumTrack, ProviderId, SourceIdentity, SourceScope, Track, TrackMetadata,
    },
    features::music_library::test_support::{
        CatalogueWrite, LibraryRead, SourceFileFacts, SourceLookup, StateStore,
    },
    platform::request::{RequestContext, RequestId},
};

const WITHIN_BUDGET: Duration = Duration::from_secs(10);
const BEYOND_BUDGET: Duration = Duration::ZERO;

struct Fixture {
    repository: Arc<PostgresTrackRepository>,
    root: PathBuf,
}

impl Fixture {
    fn new(pool: PgPool) -> Self {
        let root = std::env::temp_dir().join(format!("whio-dedup-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        Self {
            repository: Arc::new(PostgresTrackRepository::new(pool)),
            root,
        }
    }

    async fn local(&self, title: &str, name: &str) -> Track {
        std::fs::write(self.root.join(name), b"test media").unwrap();
        let source = SourceIdentity::new(
            ProviderId::new("local".into()).unwrap(),
            SourceScope::Named(self.root.display().to_string()),
            name.into(),
        )
        .unwrap();
        self.repository
            .index_source(
                source,
                metadata(title),
                &SourceFileFacts {
                    path: Some(name.into()),
                    size_bytes: 10,
                    modified_at: None,
                    missing_at: None,
                    content_type: None,
                    suffix: Some("flac".into()),
                    container: None,
                    codec: None,
                    bitrate_kbps: None,
                    sample_rate_hz: None,
                    bit_depth: None,
                    channels: None,
                    facts_version: 0,
                    acoustid_fingerprint: None,
                },
            )
            .await
            .unwrap()
    }

    async fn remote(&self, title: &str) -> Track {
        let source = SourceIdentity::new(
            ProviderId::new("youtube_music".into()).unwrap(),
            SourceScope::Global,
            "remote-video".into(),
        )
        .unwrap();
        self.repository
            .get_or_create(source, metadata(title))
            .await
            .unwrap()
    }

    async fn search(
        &self,
        mode: DedupMode,
        budget: Duration,
    ) -> Vec<whio_core::features::music_library::domain::TrackHit> {
        let service = SearchService::new(
            Arc::clone(&self.repository) as Arc<dyn LibraryRead>,
            Arc::clone(&self.repository) as Arc<dyn CatalogueWrite>,
            Arc::clone(&self.repository) as Arc<dyn SourceLookup>,
            Arc::clone(&self.repository) as Arc<dyn StateStore>,
        )
        .with_dedup_mode(mode)
        .with_dedup_budget(budget);
        service
            .search(
                &SearchRequest::uniform(
                    SearchQuery::new("Song".into()).unwrap(),
                    SearchWindow::new(SearchLimit::new(25).unwrap(), SearchOffset::default()),
                ),
                None,
                &RequestContext::new(RequestId::new("dedup-test".into()).unwrap()),
            )
            .await
            .unwrap()
            .tracks
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.into(), vec!["Artist".into()], Some(240000)).unwrap()
}

#[sqlx::test]
async fn cached_remote_is_replaced_and_its_identity_and_local_album_survive(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    let local = f.local("Song", "song.flac").await;
    let remote = f.remote("Song").await;
    let album = f
        .repository
        .resolve_or_create_album(&AlbumMetadata::new("Local Album".into()).unwrap())
        .await
        .unwrap();
    f.repository
        .set_album_track(album.id(), local.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    let hits = f.search(DedupMode::Suppress, WITHIN_BUDGET).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track().id(), local.id());
    assert_eq!(hits[0].album().unwrap().title(), "Local Album");
    assert!(
        f.repository
            .find_track(remote.id())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        f.repository.find_sources(local.id()).await.unwrap().len(),
        1
    );
    assert_eq!(
        f.repository.find_sources(remote.id()).await.unwrap().len(),
        1
    );
}

#[sqlx::test]
async fn suppression_within_budget_replaces_the_remote_hit(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    let local = f.local("Song", "song.flac").await;
    let remote = f.remote("Song").await;
    let hits = f.search(DedupMode::Suppress, WITHIN_BUDGET).await;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].track().id(), local.id());
    assert!(
        f.repository
            .find_track(remote.id())
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test]
async fn suppression_beyond_budget_keeps_the_remote_hit(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    let local = f.local("Song", "song.flac").await;
    let remote = f.remote("Song").await;
    let hits = f.search(DedupMode::Suppress, BEYOND_BUDGET).await;
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().any(|hit| hit.track().id() == remote.id()));
    assert!(hits.iter().any(|hit| hit.track().id() == local.id()));
}

#[sqlx::test]
async fn off_and_shadow_are_unaffected(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    f.local("Song", "song.flac").await;
    f.remote("Song").await;
    for mode in [DedupMode::Off, DedupMode::Shadow] {
        for budget in [WITHIN_BUDGET, BEYOND_BUDGET] {
            assert_eq!(f.search(mode, budget).await.len(), 2);
        }
    }
}

#[sqlx::test]
async fn deleted_or_empty_local_file_keeps_remote_result(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    f.local("Song", "song.flac").await;
    let remote = f.remote("Song").await;
    std::fs::remove_file(f.root.join("song.flac")).unwrap();
    assert!(
        f.search(DedupMode::Suppress, WITHIN_BUDGET)
            .await
            .iter()
            .any(|hit| hit.track().id() == remote.id())
    );
    std::fs::write(f.root.join("song.flac"), b"").unwrap();
    assert!(
        f.search(DedupMode::Suppress, WITHIN_BUDGET)
            .await
            .iter()
            .any(|hit| hit.track().id() == remote.id())
    );
}

#[sqlx::test]
async fn ambiguity_and_alternate_recordings_stay_separate(pool: PgPool) {
    for title in [
        "Song (Live)",
        "Song (Acoustic)",
        "Song (Radio Edit)",
        "Song (Blue Room)",
    ] {
        let f = Fixture::new(pool.clone());
        f.local(title, "song.flac").await;
        let remote = f.remote("Song").await;
        assert!(
            f.search(DedupMode::Suppress, WITHIN_BUDGET)
                .await
                .iter()
                .any(|hit| hit.track().id() == remote.id()),
            "{title}"
        );
    }
    let f = Fixture::new(pool.clone());
    f.local("Song", "one.flac").await;
    f.local("Song", "two.flac").await;
    let remote = f.remote("Song").await;
    assert!(
        f.search(DedupMode::Suppress, WITHIN_BUDGET)
            .await
            .iter()
            .any(|hit| hit.track().id() == remote.id())
    );
}

struct OneResult;

#[async_trait::async_trait]
impl whio_core::features::music_library::catalogue::CatalogueProvider for OneResult {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<
        Vec<whio_core::features::music_library::catalogue::CatalogueCandidate>,
        whio_core::features::music_library::catalogue::ProviderError,
    > {
        let source = SourceIdentity::new(
            ProviderId::new("youtube_music".into()).unwrap(),
            SourceScope::Global,
            "fresh-video".into(),
        )
        .unwrap();
        Ok(vec![
            whio_core::features::music_library::catalogue::CatalogueCandidate::new(
                source,
                metadata("Song"),
            )
            .with_album(Some(
                AlbumMetadata::new("Remote Compilation".into()).unwrap(),
            )),
        ])
    }
}

#[sqlx::test]
async fn fresh_provider_result_uses_local_state_without_changing_album_or_sources(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    let local = f.local("Song", "song.flac").await;
    let album = f
        .repository
        .resolve_or_create_album(&AlbumMetadata::new("Local Album".into()).unwrap())
        .await
        .unwrap();
    f.repository
        .set_album_track(album.id(), local.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();
    let user = whio_core::features::identity::provision(pool.clone())
        .create_user("dedup-user", "Dedup User")
        .await
        .unwrap();
    f.repository
        .set_rating(
            user,
            whio_core::features::music_library::domain::StateTarget::Track(local.id()),
            Some(5),
        )
        .await
        .unwrap();
    let service = SearchService::with_providers(
        Arc::clone(&f.repository) as Arc<dyn LibraryRead>,
        Arc::clone(&f.repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(&f.repository) as Arc<dyn SourceLookup>,
        Arc::clone(&f.repository) as Arc<dyn StateStore>,
        vec![Arc::new(OneResult)],
    )
    .with_dedup_budget(WITHIN_BUDGET);
    let result = service
        .search(
            &SearchRequest::uniform(
                SearchQuery::new("Song".into()).unwrap(),
                SearchWindow::new(SearchLimit::new(25).unwrap(), SearchOffset::default()),
            ),
            Some(&user),
            &RequestContext::new(RequestId::new("fresh".into()).unwrap()),
        )
        .await
        .unwrap();
    assert_eq!(result.tracks.len(), 1);
    assert_eq!(result.tracks[0].track().id(), local.id());
    assert_eq!(result.tracks[0].state().rating(), Some(5));
    assert_eq!(result.tracks[0].album().unwrap().title(), "Local Album");
    assert_eq!(
        f.repository
            .find_album_for_track(local.id())
            .await
            .unwrap()
            .unwrap()
            .title(),
        "Local Album"
    );
    assert_eq!(
        f.repository.find_sources(local.id()).await.unwrap().len(),
        1
    );
}

#[sqlx::test]
async fn generic_demo_version_does_not_bypass_the_demo_veto(pool: PgPool) {
    let f = Fixture::new(pool.clone());
    f.local("Song (Demo Version)", "song.flac").await;
    let remote = f.remote("Song (Demo Version)").await;
    assert!(
        f.search(DedupMode::Suppress, WITHIN_BUDGET)
            .await
            .iter()
            .any(|hit| hit.track().id() == remote.id())
    );
}
