use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};

use bytes::Bytes;
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    artwork::{
        ArtworkAssetDescriptor, ArtworkCache, ArtworkError, ArtworkRole, ArtworkVariant,
        FetchedImage, PromotionOutcome, RefusalReason,
    },
    domain::{ArtistId, ProviderId, SourceIdentity, SourceScope},
    storage::{contracts::ArtworkStore, postgres::PostgresTrackRepository},
};

const ROLE: ArtworkRole = ArtworkRole::Portrait;
fn scratch() -> PathBuf {
    std::env::temp_dir().join("lumen-mi04a-cache")
}

struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let root = scratch().join(format!("{name}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();

        Self { root }
    }

    fn cleared(name: &str) -> Self {
        let root = scratch().join(format!("{name}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();

        Self { root }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn repository(pool: &PgPool) -> Arc<PostgresTrackRepository> {
    Arc::new(PostgresTrackRepository::new(pool.clone()))
}

fn cache(
    store: &Arc<PostgresTrackRepository>,
    root: Option<PathBuf>,
    budget_bytes: u64,
) -> ArtworkCache {
    ArtworkCache::new(
        Arc::clone(store) as Arc<dyn ArtworkStore>,
        root,
        budget_bytes,
    )
}

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn fetched(content_type: &str, bytes: Vec<u8>) -> FetchedImage {
    FetchedImage {
        content_type: content_type.to_owned(),
        bytes: Bytes::from(bytes),
    }
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 64])
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .unwrap();

    encoded.into_inner()
}

fn gray_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::GrayImage::from_pixel(width, height, image::Luma([128]));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .unwrap();

    encoded.into_inner()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(bytes);

    hex::encode(hasher.finalize())
}

async fn rows(store: &PostgresTrackRepository, subject: &ArtistId) -> Vec<ArtworkAssetDescriptor> {
    store.find_ready(subject, ROLE).await.unwrap()
}

fn entries(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect()
}

fn total(rows: &[ArtworkAssetDescriptor]) -> u64 {
    rows.iter()
        .map(|row| u64::try_from(row.byte_size).unwrap())
        .sum()
}

#[sqlx::test]
async fn promotion_writes_every_variant_and_ready_serves_the_nearest_without_upscaling(
    pool: PgPool,
) {
    let store = repository(&pool);
    let scratch = Scratch::new("variants");
    let artist = ArtistId::new();
    let cache = cache(&store, Some(scratch.root.clone()), u64::MAX);
    let bytes = jpeg(2000, 1000);

    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-1"),
            "https://img/1.jpg",
            fetched("image/jpeg", bytes.clone()),
        )
        .await
        .unwrap();

    assert_eq!(outcome, PromotionOutcome::Promoted { revision: 1 });

    let ready = rows(&store, &artist).await;

    assert_eq!(ready.len(), 4);
    let mut variants = ready
        .iter()
        .map(|row| row.variant.as_str())
        .collect::<Vec<_>>();
    variants.sort_unstable();
    assert_eq!(variants, ["original", "px1600", "px256", "px768"]);

    let original = ready
        .iter()
        .find(|row| row.variant == ArtworkVariant::Original)
        .unwrap();
    assert_eq!(original.content_digest, sha256_hex(&bytes));
    assert_eq!(original.byte_size, bytes.len() as i64);
    assert_eq!((original.width, original.height), (2000, 1000));
    assert_eq!(original.content_type, "image/jpeg");

    let px256 = ready
        .iter()
        .find(|row| row.variant == ArtworkVariant::Px256)
        .unwrap();
    assert_eq!((px256.width, px256.height), (256, 128));

    for row in &ready {
        let path = scratch.root.join(&row.file_name);
        let stored = std::fs::read(&path).unwrap();
        assert_eq!(stored.len() as i64, row.byte_size, "{}", row.file_name);
        assert_eq!(row.content_digest, sha256_hex(&stored), "{}", row.file_name);
    }

    let small = cache
        .ready(&artist, ROLE, Some(200))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(small.descriptor.variant, ArtworkVariant::Px256);
    assert_eq!(
        small.bytes,
        Bytes::from(std::fs::read(scratch.root.join(&px256.file_name)).unwrap())
    );
    assert_eq!(small.etag, format!("\"{}-w200\"", px256.content_digest));

    let huge = cache
        .ready(&artist, ROLE, Some(5000))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(huge.descriptor.variant, ArtworkVariant::Original);
    assert_eq!(huge.bytes, Bytes::from(bytes));
    assert_eq!(huge.etag, format!("\"{}-w5000\"", original.content_digest));

    let original_size = cache.ready(&artist, ROLE, None).await.unwrap().unwrap();

    assert_eq!(original_size.descriptor.variant, ArtworkVariant::Original);
    assert_eq!(
        original_size.etag,
        format!("\"{}\"", original.content_digest)
    );
}

#[sqlx::test]
async fn a_source_smaller_than_a_target_produces_no_variant_for_it(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("small-source");
    let artist = ArtistId::new();
    let cache = cache(&store, Some(scratch.root.clone()), u64::MAX);

    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-2"),
            "https://img/2.jpg",
            fetched("image/jpeg", jpeg(200, 100)),
        )
        .await
        .unwrap();

    assert_eq!(outcome, PromotionOutcome::Promoted { revision: 1 });

    let ready = rows(&store, &artist).await;

    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].variant, ArtworkVariant::Original);

    let large = cache
        .ready(&artist, ROLE, Some(768))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(large.descriptor.variant, ArtworkVariant::Original);
    assert_eq!(large.descriptor.width, 200);
}

#[sqlx::test]
async fn a_refusal_writes_no_file_and_no_descriptor(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("refusals");
    let enabled_cache = cache(&store, Some(scratch.root.clone()), u64::MAX);

    let cases = [
        (
            "undecodable",
            fetched("image/jpeg", b"not an image".to_vec()),
            RefusalReason::Undecodable,
        ),
        (
            "too-many-bytes",
            fetched("image/jpeg", vec![7; 9 * 1024 * 1024]),
            RefusalReason::TooManyBytes,
        ),
        (
            "too-many-pixels",
            fetched("image/jpeg", gray_jpeg(4100, 4000)),
            RefusalReason::TooManyPixels,
        ),
        (
            "non-image-content-type",
            fetched("application/octet-stream", jpeg(400, 300)),
            RefusalReason::Undecodable,
        ),
    ];

    for (index, (label, image, reason)) in cases.into_iter().enumerate() {
        let subject = ArtistId::new();
        let outcome = enabled_cache
            .promote(
                &subject,
                ROLE,
                &source(&format!("channel-{index}")),
                "https://img/x.jpg",
                image,
            )
            .await
            .unwrap();

        assert_eq!(outcome, PromotionOutcome::Refused(reason), "{label}");
        assert!(rows(&store, &subject).await.is_empty(), "{label}");
    }

    assert!(entries(&scratch.root).is_empty());

    let disabled = ArtistId::new();
    let disabled_cache = cache(&store, None, u64::MAX);
    let outcome = disabled_cache
        .promote(
            &disabled,
            ROLE,
            &source("channel-none"),
            "https://img/x.jpg",
            fetched("image/jpeg", jpeg(400, 300)),
        )
        .await
        .unwrap();

    assert!(rows(&store, &disabled).await.is_empty());
    assert!(entries(&scratch.root).is_empty());
    assert_eq!(
        outcome,
        PromotionOutcome::Refused(RefusalReason::NoCacheDir)
    );
}

#[sqlx::test]
async fn a_failed_stage_leaves_no_descriptor_and_no_final_file(pool: PgPool) {
    let store = repository(&pool);
    let artist = ArtistId::new();
    let missing = scratch().join(format!("missing-{}", Uuid::now_v7()));
    let cache = cache(&store, Some(missing.clone()), u64::MAX);

    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-3"),
            "https://img/3.jpg",
            fetched("image/jpeg", jpeg(800, 400)),
        )
        .await;

    assert!(matches!(outcome, Err(ArtworkError::Unavailable)));
    assert!(rows(&store, &artist).await.is_empty());
    assert!(!missing.exists());
}

#[sqlx::test]
async fn a_read_only_cache_root_is_refused_with_nothing_written(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::cleared("read-only");
    let artist = ArtistId::new();
    let cache = cache(&store, Some(scratch.root.clone()), u64::MAX);

    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-4"),
            "https://img/4.jpg",
            fetched("image/jpeg", jpeg(800, 400)),
        )
        .await;

    assert!(matches!(outcome, Err(ArtworkError::Unavailable)));
    assert!(rows(&store, &artist).await.is_empty());
    assert!(entries(&scratch.root).is_empty());
}

#[sqlx::test]
async fn a_second_image_for_the_same_subject_becomes_revision_two(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("revision");
    let artist = ArtistId::new();
    let cache = cache(&store, Some(scratch.root.clone()), u64::MAX);
    let first = jpeg(400, 200);
    let second = jpeg(2000, 1000);

    cache
        .promote(
            &artist,
            ROLE,
            &source("channel-5"),
            "https://img/5.jpg",
            fetched("image/jpeg", first.clone()),
        )
        .await
        .unwrap();
    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-5"),
            "https://img/5.jpg",
            fetched("image/jpeg", second.clone()),
        )
        .await
        .unwrap();

    assert_eq!(outcome, PromotionOutcome::Promoted { revision: 2 });

    let ready = cache
        .ready(&artist, ROLE, Some(5000))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(ready.descriptor.revision, 2);
    assert_eq!(ready.descriptor.content_digest, sha256_hex(&second));
    assert_eq!(ready.bytes, Bytes::from(second));

    let all = rows(&store, &artist).await;
    let first_revision = all
        .iter()
        .filter(|row| row.revision == 1)
        .map(|row| row.id)
        .collect::<Vec<_>>();

    assert!(!first_revision.is_empty());
    let evictable = store.lru_descriptors(None, 100).await.unwrap();

    assert!(
        first_revision
            .iter()
            .all(|id| evictable.iter().any(|row| row.id == *id))
    );
}

#[sqlx::test]
async fn promoting_the_same_reference_again_is_already_ready(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("already-ready");
    let artist = ArtistId::new();
    let cache = cache(&store, Some(scratch.root.clone()), u64::MAX);
    let bytes = jpeg(800, 400);

    cache
        .promote(
            &artist,
            ROLE,
            &source("channel-6"),
            "https://img/6.jpg",
            fetched("image/jpeg", bytes.clone()),
        )
        .await
        .unwrap();
    let outcome = cache
        .promote(
            &artist,
            ROLE,
            &source("channel-6"),
            "https://img/6.jpg",
            fetched("image/jpeg", bytes),
        )
        .await
        .unwrap();

    assert_eq!(outcome, PromotionOutcome::AlreadyReady);
    assert!(
        rows(&store, &artist)
            .await
            .iter()
            .all(|row| row.revision == 1)
    );
}

#[sqlx::test]
async fn the_budget_evicts_the_least_recently_used_asset_and_keeps_the_promoted_one(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("budget");
    let oldest_but_used = ArtistId::new();
    let stale = ArtistId::new();
    let promoted = ArtistId::new();
    let generous = cache(&store, Some(scratch.root.clone()), u64::MAX);

    generous
        .promote(
            &oldest_but_used,
            ROLE,
            &source("channel-a"),
            "https://img/a.jpg",
            fetched("image/jpeg", jpeg(100, 80)),
        )
        .await
        .unwrap();
    generous
        .promote(
            &stale,
            ROLE,
            &source("channel-b"),
            "https://img/b.jpg",
            fetched("image/jpeg", jpeg(120, 90)),
        )
        .await
        .unwrap();

    let used = rows(&store, &oldest_but_used).await;
    let stale_rows = rows(&store, &stale).await;

    assert!(
        generous
            .ready(&oldest_but_used, ROLE, None)
            .await
            .unwrap()
            .is_some()
    );

    let smallest = jpeg(32, 32);
    assert!(smallest.len() as u64 <= total(&stale_rows));

    let budget = total(&used) + total(&stale_rows);
    let tight = cache(&store, Some(scratch.root.clone()), budget);
    let outcome = tight
        .promote(
            &promoted,
            ROLE,
            &source("channel-c"),
            "https://img/c.jpg",
            fetched("image/jpeg", smallest),
        )
        .await
        .unwrap();

    assert_eq!(outcome, PromotionOutcome::Promoted { revision: 1 });
    assert!(rows(&store, &stale).await.is_empty());

    for row in &stale_rows {
        assert!(
            !scratch.root.join(&row.file_name).exists(),
            "{}",
            row.file_name
        );
    }

    for row in rows(&store, &oldest_but_used).await {
        assert!(
            scratch.root.join(&row.file_name).exists(),
            "{}",
            row.file_name
        );
    }

    let kept = rows(&store, &promoted).await;

    assert!(!kept.is_empty());
    for row in &kept {
        assert!(
            scratch.root.join(&row.file_name).exists(),
            "{}",
            row.file_name
        );
    }
    assert!(tight.ready(&promoted, ROLE, None).await.unwrap().is_some());
}

#[sqlx::test]
async fn a_restart_over_the_same_root_and_database_serves_the_ready_asset(pool: PgPool) {
    let store = repository(&pool);
    let scratch = Scratch::new("restart");
    let artist = ArtistId::new();
    let bytes = jpeg(1200, 600);

    cache(&store, Some(scratch.root.clone()), u64::MAX)
        .promote(
            &artist,
            ROLE,
            &source("channel-7"),
            "https://img/7.jpg",
            fetched("image/jpeg", bytes.clone()),
        )
        .await
        .unwrap();

    let before = rows(&store, &artist).await;
    let restarted = cache(&store, Some(scratch.root.clone()), u64::MAX);
    let ready = restarted
        .ready(&artist, ROLE, Some(600))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(ready.descriptor.revision, 1);
    assert_eq!(ready.descriptor.variant, ArtworkVariant::Px768);
    assert_eq!(
        ready.bytes,
        Bytes::from(std::fs::read(scratch.root.join(&ready.descriptor.file_name)).unwrap())
    );
    assert_eq!(rows(&store, &artist).await.len(), before.len());
    assert_eq!(
        before
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        before.len()
    );
}
