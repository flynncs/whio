use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use axum::{
    Router,
    http::{StatusCode, header},
    routing::get,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::{
    artwork::{
        ArtworkAssetDescriptor, ArtworkCache, ArtworkError, ArtworkOutcome, ArtworkRole,
        ArtworkVariant, CoverArtTarget, PortraitGate, PromotionOutcome, RefusalReason,
        test_support::Ports, thumbnail,
    },
    domain::{
        Album, AlbumEntry, AlbumId, AlbumSummary, ArtistId, ArtworkRef, ProviderId, SourceIdentity,
        SourceScope, TrackId,
    },
    storage::contracts::{
        ArtworkStore, LibraryRead, SourceFileFacts, SourceLookup, StateStore, TrackRepositoryError,
    },
};

const SCRATCH: &str = "/private/tmp/lumen-mi04b-cache";
const PORTRAIT: ArtworkRole = ArtworkRole::Portrait;
const WIDE: ArtworkRole = ArtworkRole::Wide;

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(SCRATCH).join(format!("{name}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();

        Self { root }
    }

    fn entries(&self) -> usize {
        std::fs::read_dir(&self.root).unwrap().count()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 96])
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .unwrap();

    encoded.into_inner()
}

fn album_summary() -> AlbumSummary {
    AlbumSummary::new(
        Album::new(
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
        None,
        0,
        0,
        Utc::now(),
    )
}

struct Assets {
    rows: Mutex<Vec<ArtworkAssetDescriptor>>,
}

impl Assets {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            rows: Mutex::new(Vec::new()),
        })
    }

    fn add(&self, subject: &ArtistId, role: ArtworkRole, variant: ArtworkVariant, file_name: &str) {
        self.add_digest(subject, role, variant, file_name, &"a".repeat(64));
    }

    fn add_digest(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
        variant: ArtworkVariant,
        file_name: &str,
        content_digest: &str,
    ) {
        self.rows.lock().unwrap().push(ArtworkAssetDescriptor {
            id: Uuid::now_v7(),
            subject: subject.clone(),
            role,
            source_provider_id: "youtube_music".to_owned(),
            source_external_id: "channel-1".to_owned(),
            revision: 1,
            variant,
            content_digest: content_digest.to_owned(),
            content_type: "image/jpeg".to_owned(),
            width: 640,
            height: 640,
            byte_size: 4,
            file_name: file_name.to_owned(),
        });
    }

    fn digest(&self, index: usize) -> String {
        self.rows.lock().unwrap()[index].content_digest.clone()
    }

    fn rows(&self) -> Vec<ArtworkAssetDescriptor> {
        self.rows.lock().unwrap().clone()
    }
}

#[async_trait]
impl ArtworkStore for Assets {
    async fn find_ready(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        Ok(self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|row| &row.subject == subject && row.role == role)
            .cloned()
            .collect())
    }

    async fn record_asset(
        &self,
        _tx: &mut PgConnection,
        _asset: &ArtworkAssetDescriptor,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn record_assets(
        &self,
        assets: &[ArtworkAssetDescriptor],
    ) -> Result<(), TrackRepositoryError> {
        self.rows.lock().unwrap().extend_from_slice(assets);
        Ok(())
    }

    async fn touch(&self, _id: Uuid, _now: DateTime<Utc>) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn lru_descriptors(
        &self,
        _exclude: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        Ok(self.rows.lock().unwrap().clone())
    }

    async fn delete_asset(&self, _id: Uuid) -> Result<bool, TrackRepositoryError> {
        Ok(false)
    }
}

#[derive(Default)]
struct Walk {
    albums: Mutex<VecDeque<Vec<AlbumEntry>>>,
    album_calls: AtomicUsize,
    registered: AtomicUsize,
}

impl Walk {
    fn with_albums(count: usize) -> Arc<Self> {
        let walk = Self::default();
        for _ in 0..count {
            walk.register_album();
        }

        Arc::new(walk)
    }

    fn register_album(&self) {
        self.albums.lock().unwrap().push_back(Vec::new());
        self.registered.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl LibraryRead for Walk {
    async fn find_album_entries(
        &self,
        _album_id: &AlbumId,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        self.album_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.albums.lock().unwrap().pop_front().unwrap_or_default())
    }

    async fn find_albums_for_artist(
        &self,
        _artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        let count = self.registered.load(Ordering::SeqCst);

        Ok((0..count).map(|_| album_summary()).collect())
    }

    async fn find_track(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<crate::features::music_library::domain::Track>, TrackRepositoryError> {
        Ok(None)
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: crate::features::music_library::domain::TrackOrder,
        _filter: &crate::features::music_library::domain::TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<crate::features::music_library::domain::TrackPage, TrackRepositoryError> {
        Ok(crate::features::music_library::domain::TrackPage::new(
            Vec::new(),
            0,
        ))
    }

    async fn track_views(
        &self,
        _track_ids: &[TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<crate::features::music_library::domain::TrackView>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl SourceLookup for Walk {
    async fn find_sources(
        &self,
        _track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_source_file_facts(
        &self,
        _source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_artwork(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        Ok(None)
    }
}

impl StateStore for Walk {}

struct Server {
    base: String,
    requests: Arc<AtomicUsize>,
}

impl Drop for Server {
    fn drop(&mut self) {
        assert_eq!(
            self.requests.load(Ordering::SeqCst),
            0,
            "the artwork path contacted {}",
            self.base
        );
    }
}

async fn server(bytes: &'static [u8]) -> Server {
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&requests);
    let app = Router::new().route(
        "/cover.jpg",
        get(move || {
            let counter = Arc::clone(&counter);
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "image/jpeg")],
                    bytes,
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    Server { base, requests }
}

fn suffix_of(base: &str) -> String {
    reqwest::Url::parse(base)
        .unwrap()
        .host_str()
        .unwrap()
        .to_owned()
}

fn cache(assets: &Arc<Assets>, root: Option<PathBuf>) -> Arc<ArtworkCache> {
    Arc::new(ArtworkCache::new(
        Arc::clone(assets) as Arc<dyn ArtworkStore>,
        root,
        u64::MAX,
    ))
}

fn ports(walk: &Arc<Walk>) -> Ports {
    Ports::new(
        Arc::clone(walk) as Arc<dyn LibraryRead>,
        Arc::clone(walk) as Arc<dyn SourceLookup>,
        Arc::clone(walk) as Arc<dyn StateStore>,
    )
}

fn service_with(
    walk: &Arc<Walk>,
    assets: &Arc<Assets>,
    scratch: &Scratch,
    host_suffixes: Vec<String>,
) -> crate::features::music_library::artwork::ArtworkService {
    ports(walk)
        .artwork(host_suffixes)
        .with_cache(cache(assets, Some(scratch.root.clone())))
}

#[tokio::test]
async fn a_ready_portrait_is_served_without_a_network_call_or_a_resize() {
    let scratch = Scratch::new("ready-portrait");
    let server = server(b"never-fetched").await;
    let assets = Assets::new();
    let artist = ArtistId::new();
    let original = jpeg(2000, 2000);
    let px256 = jpeg(256, 256);
    let px768 = jpeg(768, 768);
    std::fs::write(scratch.root.join("original.jpg"), &original).unwrap();
    std::fs::write(scratch.root.join("px256.jpg"), &px256).unwrap();
    std::fs::write(scratch.root.join("px768.jpg"), &px768).unwrap();
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Original,
        "original.jpg",
        "original-digest",
    );
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Px256,
        "px256.jpg",
        "px256-digest",
    );
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Px768,
        "px768.jpg",
        "px768-digest",
    );
    let walk = Walk::with_albums(1);
    let service = service_with(&walk, &assets, &scratch, vec![suffix_of(&server.base)]);

    assert_ne!(
        thumbnail::fit(&original, 256).unwrap(),
        Bytes::from(px256.clone())
    );
    assert_ne!(
        thumbnail::fit(&original, 768).unwrap(),
        Bytes::from(px768.clone())
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist.clone()), None, Some(256))
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };

    assert!(artwork.fallback().is_none());
    let (content_type, served, etag) = artwork.into_parts();
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(served.len(), px256.len());
    assert_eq!(served, Bytes::from(px256.clone()));
    assert_eq!(etag, "\"px256-digest-w256\"");

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist), None, Some(768))
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };
    let (_, served, etag) = artwork.into_parts();
    assert_eq!(served, Bytes::from(px768.clone()));
    assert_eq!(etag, "\"px768-digest-w768\"");
    assert_ne!(px256.len(), px768.len());
    assert_eq!(scratch.entries(), 3);

    drop(server);
}

#[tokio::test]
async fn a_ready_portrait_serves_the_stored_bytes_unchanged_when_no_size_is_asked_for() {
    let scratch = Scratch::new("ready-unsized");
    let assets = Assets::new();
    let artist = ArtistId::new();
    let bytes = jpeg(2000, 2000);
    std::fs::write(scratch.root.join("portrait.jpg"), &bytes).unwrap();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "portrait.jpg");
    let walk = Walk::with_albums(1);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist.clone()), None, None)
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };
    let (_, served, etag) = artwork.into_parts();

    assert_eq!(served, Bytes::from(bytes));
    assert_eq!(etag, format!("\"{}\"", assets.digest(0)));

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), Some(&etag), None)
            .await
            .unwrap(),
        ArtworkOutcome::NotModified(returned) if returned == etag
    ));
}

#[tokio::test]
async fn a_ready_portrait_that_already_fits_is_not_resized() {
    let scratch = Scratch::new("ready-small");
    let assets = Assets::new();
    let artist = ArtistId::new();
    let bytes = jpeg(200, 200);
    std::fs::write(scratch.root.join("portrait.jpg"), &bytes).unwrap();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "portrait.jpg");
    let walk = Walk::with_albums(1);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist.clone()), None, Some(256))
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };
    let (_, served, etag) = artwork.into_parts();

    assert_eq!(served, Bytes::from(bytes));
    assert_eq!(etag, format!("\"{}-w256\"", assets.digest(0)));

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), Some(&etag), Some(256))
            .await
            .unwrap(),
        ArtworkOutcome::NotModified(returned) if returned == etag
    ));
}

#[tokio::test]
async fn a_sized_portrait_cannot_revalidate_against_another_sizes_etag() {
    let scratch = Scratch::new("ready-sized-etag");
    let assets = Assets::new();
    let artist = ArtistId::new();
    let px256 = jpeg(256, 256);
    let px1600 = jpeg(1600, 1600);
    std::fs::write(scratch.root.join("px256.jpg"), &px256).unwrap();
    std::fs::write(scratch.root.join("px1600.jpg"), &px1600).unwrap();
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Px256,
        "px256.jpg",
        "px256-digest",
    );
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Px1600,
        "px1600.jpg",
        "px1600-digest",
    );
    let walk = Walk::with_albums(0);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(large) = service
        .load(CoverArtTarget::Artist(artist.clone()), None, Some(1200))
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };
    let (_, large_bytes, large_etag) = large.into_parts();

    let ArtworkOutcome::Found(small) = service
        .load(
            CoverArtTarget::Artist(artist.clone()),
            Some(&large_etag),
            Some(64),
        )
        .await
        .unwrap()
    else {
        panic!("a 64px request revalidated against the 1200px etag {large_etag}");
    };
    let (_, small_bytes, small_etag) = small.into_parts();

    assert_eq!(large_bytes, Bytes::from(px1600.clone()));
    assert_eq!(large_etag, "\"px1600-digest-w1200\"");
    assert_eq!(small_bytes, Bytes::from(px256));
    assert_eq!(small_etag, "\"px256-digest-w64\"");
    assert_ne!(large_etag, small_etag);
}

#[tokio::test]
async fn the_same_sized_portrait_request_still_revalidates() {
    let scratch = Scratch::new("ready-same-size");
    let assets = Assets::new();
    let artist = ArtistId::new();
    let px256 = jpeg(256, 256);
    std::fs::write(scratch.root.join("px256.jpg"), &px256).unwrap();
    assets.add_digest(
        &artist,
        PORTRAIT,
        ArtworkVariant::Px256,
        "px256.jpg",
        "px256-digest",
    );
    let walk = Walk::with_albums(0);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist.clone()), None, Some(256))
        .await
        .unwrap()
    else {
        panic!("expected the ready portrait");
    };
    let (_, served, etag) = artwork.into_parts();
    assert_eq!(served, Bytes::from(px256));
    assert_eq!(etag, "\"px256-digest-w256\"");

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), Some(&etag), Some(256))
            .await
            .unwrap(),
        ArtworkOutcome::NotModified(returned) if returned == etag
    ));
}

#[tokio::test]
async fn a_wide_role_is_served_as_a_labelled_fallback_without_the_album_walk() {
    let scratch = Scratch::new("ready-wide");
    let assets = Assets::new();
    let artist = ArtistId::new();
    std::fs::write(scratch.root.join("wide.jpg"), b"wide-bytes").unwrap();
    assets.add(&artist, WIDE, ArtworkVariant::Original, "wide.jpg");
    let walk = Walk::with_albums(1);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist), None, None)
        .await
        .unwrap()
    else {
        panic!("expected the wide fallback");
    };

    assert_eq!(artwork.fallback(), Some("wide"));
    let (_, served, _) = artwork.into_parts();
    assert_eq!(served, Bytes::from_static(b"wide-bytes"));
}

#[tokio::test]
async fn a_ready_portrait_outranks_a_ready_wide_role() {
    let scratch = Scratch::new("portrait-first");
    let assets = Assets::new();
    let artist = ArtistId::new();
    std::fs::write(scratch.root.join("portrait.jpg"), b"portrait-bytes").unwrap();
    std::fs::write(scratch.root.join("wide.jpg"), b"wide-bytes").unwrap();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "portrait.jpg");
    assets.add(&artist, WIDE, ArtworkVariant::Original, "wide.jpg");
    let walk = Walk::with_albums(1);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Artist(artist), None, None)
        .await
        .unwrap()
    else {
        panic!("expected the portrait");
    };

    assert!(artwork.fallback().is_none());
    let (_, served, _) = artwork.into_parts();
    assert_eq!(served, Bytes::from_static(b"portrait-bytes"));
}

#[tokio::test]
async fn an_artist_with_no_ready_asset_and_no_albums_is_missing() {
    let scratch = Scratch::new("bare-artist");
    let assets = Assets::new();
    let walk = Walk::with_albums(0);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(ArtistId::new()), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
}

#[tokio::test]
async fn a_cached_artist_with_albums_and_no_ready_asset_is_missing() {
    let scratch = Scratch::new("walk-skipped");
    let assets = Assets::new();
    let walk = Walk::with_albums(2);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(ArtistId::new()), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
}

#[tokio::test]
async fn a_cached_artist_without_a_ready_asset_stays_missing() {
    let scratch = Scratch::new("walk-later");
    let assets = Assets::new();
    let artist = ArtistId::new();
    let walk = Walk::with_albums(0);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist.clone()), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));

    walk.register_album();

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
}

#[tokio::test]
async fn an_evicted_portrait_whose_file_is_gone_falls_back_to_the_album_walk() {
    let scratch = Scratch::new("evicted");
    let assets = Assets::new();
    let artist = ArtistId::new();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "gone.jpg");
    let stored = cache(&assets, Some(scratch.root.clone()));

    assert!(matches!(
        stored.ready(&artist, PORTRAIT, None).await,
        Err(ArtworkError::Unavailable)
    ));

    let walk = Walk::with_albums(0);
    let service = ports(&walk)
        .artwork(crate::features::music_library::artwork::default_host_suffixes())
        .with_cache(stored);

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
}

#[tokio::test]
async fn an_evicted_wide_file_falls_through_to_the_album_walk_rather_than_erroring() {
    let scratch = Scratch::new("evicted-wide");
    let assets = Assets::new();
    let artist = ArtistId::new();
    assets.add(&artist, WIDE, ArtworkVariant::Original, "gone.jpg");
    let walk = Walk::with_albums(0);
    let service = ports(&walk)
        .artwork(crate::features::music_library::artwork::default_host_suffixes())
        .with_cache(cache(&assets, Some(scratch.root.clone())));

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
    assert_eq!(scratch.entries(), 0);
}

#[tokio::test]
async fn a_corrupt_ready_asset_does_not_mask_a_newer_ready_portrait() {
    let scratch = Scratch::new("corrupt");
    let assets = Assets::new();
    let artist = ArtistId::new();
    std::fs::write(scratch.root.join("corrupt.jpg"), b"not an image").unwrap();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "corrupt.jpg");
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "corrupt.jpg");
    let walk = Walk::with_albums(1);
    let service = service_with(
        &walk,
        &assets,
        &scratch,
        crate::features::music_library::artwork::default_host_suffixes(),
    );

    let outcome = service
        .load(CoverArtTarget::Artist(artist), None, Some(256))
        .await;

    assert!(matches!(outcome, Ok(ArtworkOutcome::Found(_))));
}

#[tokio::test]
async fn without_a_cache_the_artist_walk_is_the_only_source() {
    let scratch = Scratch::new("no-cache");
    let assets = Assets::new();
    let artist = ArtistId::new();
    std::fs::write(scratch.root.join("portrait.jpg"), b"portrait-bytes").unwrap();
    assets.add(&artist, PORTRAIT, ArtworkVariant::Original, "portrait.jpg");
    let walk = Walk::with_albums(0);
    let service =
        ports(&walk).artwork(crate::features::music_library::artwork::default_host_suffixes());

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(artist), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
    assert_eq!(scratch.entries(), 1);
}

#[tokio::test]
async fn a_cached_artist_with_albums_still_never_fetches_a_metadata_portrait() {
    let scratch = Scratch::new("no-inline");
    let server = server(b"never-fetched").await;
    let assets = Assets::new();
    let walk = Walk::with_albums(3);
    let service = service_with(&walk, &assets, &scratch, vec![suffix_of(&server.base)]);

    assert!(matches!(
        service
            .load(CoverArtTarget::Artist(ArtistId::new()), None, None)
            .await,
        Ok(ArtworkOutcome::Missing)
    ));
    assert_eq!(scratch.entries(), 0);

    drop(server);
}

#[tokio::test]
async fn reference_only_refuses_before_any_network_call() {
    let scratch = Scratch::new("reference-only");
    let server = server(b"never-fetched").await;
    let assets = Assets::new();
    let walk = Walk::with_albums(0);
    let service = service_with(&walk, &assets, &scratch, vec!["127.0.0.1".to_owned()]);
    let reference = format!("{}/cover.jpg", server.base);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            &reference,
            PortraitGate::ReferenceOnly,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::NotServable)
    );
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);

    drop(server);
}

#[tokio::test]
async fn an_unset_cache_refuses_promotion_before_any_network_call() {
    let server = server(b"never-fetched").await;
    let walk = Walk::with_albums(0);
    let service = ports(&walk).artwork(vec![suffix_of(&server.base)]);
    let reference = format!("{}/cover.jpg", server.base);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            &reference,
            PortraitGate::Servable,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::NoCacheDir)
    );

    drop(server);
}

#[tokio::test]
async fn a_reference_outside_the_host_allowlist_is_refused_before_fetching() {
    let scratch = Scratch::new("disallowed");
    let assets = Assets::new();
    let walk = Walk::with_albums(0);
    let service = service_with(&walk, &assets, &scratch, vec!["ytimg.com".to_owned()]);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            "https://evil.example/cover.jpg",
            PortraitGate::Servable,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::DisallowedReference)
    );
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);
}

#[tokio::test]
async fn a_malformed_reference_is_a_fetch_error_and_writes_nothing() {
    let scratch = Scratch::new("malformed");
    let assets = Assets::new();
    let walk = Walk::with_albums(0);
    let service = service_with(&walk, &assets, &scratch, vec!["ytimg.com".to_owned()]);

    assert!(matches!(
        service
            .promote_artist_portrait(
                &ArtistId::new(),
                &source("channel-1"),
                "not a url",
                PortraitGate::Servable
            )
            .await,
        Err(ArtworkError::Fetch(_))
    ));
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);
}

#[tokio::test]
async fn a_plain_http_reference_is_refused_without_contacting_the_server() {
    let scratch = Scratch::new("plain-http");
    let server = server(b"not an image").await;
    let assets = Assets::new();
    let walk = Walk::with_albums(0);
    let service = service_with(&walk, &assets, &scratch, vec![suffix_of(&server.base)]);
    let reference = format!("{}/cover.jpg", server.base);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            &reference,
            PortraitGate::Servable,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::DisallowedReference)
    );
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);

    drop(server);
}

#[tokio::test]
async fn a_servable_reference_outside_the_allowlist_is_refused_without_fetching() {
    let scratch = Scratch::new("promote");
    let server = server(PROMOTED).await;
    let assets = Assets::new();
    let walk = Walk::with_albums(1);
    let service = service_with(&walk, &assets, &scratch, vec!["ytimg.com".to_owned()]);
    let reference = format!("{}/cover.jpg", server.base);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            &reference,
            PortraitGate::Servable,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::DisallowedReference)
    );
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);

    drop(server);
}

#[tokio::test]
async fn a_refusal_from_the_cache_leaves_the_reference_unpromoted() {
    let scratch = Scratch::new("refused-cache");
    let assets = Assets::new();
    let walk = Walk::with_albums(1);
    let service = service_with(&walk, &assets, &scratch, vec!["ytimg.com".to_owned()]);

    let outcome = service
        .promote_artist_portrait(
            &ArtistId::new(),
            &source("channel-1"),
            "https://evil.example/cover.jpg",
            PortraitGate::Servable,
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::DisallowedReference)
    );
    assert!(assets.rows().is_empty());
    assert_eq!(scratch.entries(), 0);
}

static PROMOTED: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];
