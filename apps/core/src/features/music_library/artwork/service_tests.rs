use super::*;
use crate::features::music_library::storage::contracts::{CatalogueWrite, SourceLookup};
use crate::features::music_library::{
    domain::{
        AlbumMetadata, AlbumTrack, ArtistCredit, ArtistId, ArtworkRef, ProviderId, SourceIdentity,
        TrackId, TrackMetadata,
    },
    storage::contracts::SourceFileFacts,
    storage::postgres::PostgresTrackRepository,
};

fn pictured_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("whio-artwork-{label}-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    crate::features::music_library::library::write_pictured_flac(
        &root.join("a.flac"),
        vec![lofty::picture::Picture::new_unchecked(
            lofty::picture::PictureType::CoverFront,
            Some(lofty::picture::MimeType::Jpeg),
            None,
            vec![0xFF, 0xD8, 0xFF, 0xE0],
        )],
    );

    root
}

fn plain_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("whio-artwork-{label}-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac"),
        root.join("a.flac"),
    )
    .unwrap();

    root
}

fn local_source(root: &std::path::Path, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named(root.display().to_string()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn remote_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn local_facts(relative: &str) -> SourceFileFacts {
    SourceFileFacts {
        path: Some(relative.to_owned()),
        size_bytes: 10,
        modified_at: None,
        missing_at: None,
        content_type: None,
        suffix: Some("flac".to_owned()),
        container: None,
        codec: None,
        bitrate_kbps: None,
        sample_rate_hz: None,
        bit_depth: None,
        channels: None,
        facts_version: 0,
        acoustid_fingerprint: None,
    }
}

fn track_metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn add_local_track(
    repository: &Arc<PostgresTrackRepository>,
    root: &std::path::Path,
    external_id: &str,
) -> TrackId {
    let source = local_source(root, external_id);
    let track = repository
        .get_or_create(source.clone(), track_metadata("Local"))
        .await
        .unwrap();
    repository
        .set_source_file_facts(&source, &local_facts("a.flac"))
        .await
        .unwrap();

    track.id().clone()
}

async fn add_stored_remote_track(
    repository: &Arc<PostgresTrackRepository>,
    external_id: &str,
) -> TrackId {
    let source = remote_source(external_id);
    let track = repository
        .get_or_create(source.clone(), track_metadata("Remote"))
        .await
        .unwrap();
    repository
        .set_source_artwork(
            &source,
            &ArtworkRef::url("https://art.test/remote.jpg".to_owned()).unwrap(),
        )
        .await
        .unwrap();

    track.id().clone()
}

async fn album_with(
    repository: &Arc<PostgresTrackRepository>,
    album_artist: Option<&str>,
    members: &[TrackId],
) -> AlbumId {
    let mut metadata = AlbumMetadata::new("Album".to_owned()).unwrap();
    if let Some(name) = album_artist {
        metadata = metadata.with_album_artist(ArtistCredit::new(name.to_owned(), None).unwrap());
    }
    let album = repository.resolve_or_create_album(&metadata).await.unwrap();
    for (index, track_id) in members.iter().enumerate() {
        repository
            .set_album_track(
                album.id(),
                track_id,
                &AlbumTrack::new(Some(1), Some(index as u16 + 1), index as u32).unwrap(),
            )
            .await
            .unwrap();
    }

    album.id().clone()
}

fn service(repository: Arc<PostgresTrackRepository>) -> ArtworkService {
    ArtworkService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&repository) as Arc<dyn SourceLookup>,
        crate::features::music_library::artwork::default_host_suffixes(),
    )
}

#[sqlx::test]
async fn an_embedded_picture_is_a_tracks_cover_art(pool: sqlx::PgPool) {
    let root = pictured_root("track");
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "recording-1").await;

    let resolved = service(repository)
        .resolve(CoverArtTarget::Track(track_id))
        .await
        .unwrap()
        .expect("embedded art");

    assert!(matches!(resolved, Resolved::Local { .. }));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn folder_artwork_uses_name_and_extension_precedence(pool: sqlx::PgPool) {
    let root = plain_root("folder-precedence");
    for (name, bytes) in [
        ("cover.jpeg", vec![0xFF, 0xD8, 0xFF, 1]),
        ("Cover.JPG", vec![0xFF, 0xD8, 0xFF, 2]),
        ("folder.jpg", vec![0xFF, 0xD8, 0xFF, 3]),
        ("front.webp", b"RIFF0000WEBP".to_vec()),
    ] {
        std::fs::write(root.join(name), bytes).unwrap();
    }
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "folder-precedence").await;

    let ArtworkOutcome::Found(artwork) = service(repository)
        .load(CoverArtTarget::Track(track_id), None, None)
        .await
        .unwrap()
    else {
        panic!("expected folder artwork");
    };
    let (content_type, bytes, _) = artwork.into_parts();
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(bytes.as_ref(), &[0xFF, 0xD8, 0xFF, 2]);

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn embedded_art_beats_folder_art_and_large_local_pictures_are_allowed(pool: sqlx::PgPool) {
    let root = pictured_root("large-local");
    std::fs::write(root.join("cover.jpg"), [0xFF, 0xD8, 0xFF, 9]).unwrap();
    crate::features::music_library::library::write_pictured_flac(
        &root.join("a.flac"),
        vec![lofty::picture::Picture::new_unchecked(
            lofty::picture::PictureType::CoverFront,
            Some(lofty::picture::MimeType::Jpeg),
            None,
            {
                let mut bytes = vec![0xFF, 0xD8, 0xFF];
                bytes.resize(9 * 1024 * 1024, 4);
                bytes
            },
        )],
    );
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "large-local").await;

    let ArtworkOutcome::Found(artwork) = service(repository)
        .load(CoverArtTarget::Track(track_id), None, None)
        .await
        .unwrap()
    else {
        panic!("expected embedded artwork");
    };
    let (content_type, bytes, _) = artwork.into_parts();
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(bytes.len(), 9 * 1024 * 1024);
    assert_eq!(bytes[3], 4);

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn album_art_walks_members_and_falls_back_to_stored_artwork(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let root = plain_root("plain");
    let plain = add_local_track(&repository, &root, "recording-plain").await;
    let remote = add_stored_remote_track(&repository, "vid-1").await;
    let album = album_with(&repository, None, &[plain, remote]).await;

    let resolved = service(repository)
        .resolve(CoverArtTarget::Album(album))
        .await
        .unwrap()
        .expect("stored remote art");

    assert!(matches!(
        resolved,
        Resolved::Remote(reference) if reference == "https://art.test/remote.jpg"
    ));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn a_bare_album_id_resolves_album_art(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let root = plain_root("bare");
    let plain = add_local_track(&repository, &root, "recording-plain").await;
    let remote = add_stored_remote_track(&repository, "vid-1").await;
    let album = album_with(&repository, None, &[plain, remote]).await;

    let resolved = service(repository)
        .resolve(CoverArtTarget::Track(TrackId::from_uuid(*album.as_uuid())))
        .await
        .unwrap()
        .expect("stored remote art");

    assert!(matches!(
        resolved,
        Resolved::Remote(reference) if reference == "https://art.test/remote.jpg"
    ));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn album_art_prefers_embedded_art_over_an_earlier_stored_thumbnail(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let remote = add_stored_remote_track(&repository, "vid-1").await;
    let root = pictured_root("prefer-local");
    let local = add_local_track(&repository, &root, "recording-1").await;
    let album = album_with(&repository, None, &[remote, local]).await;

    let resolved = service(repository)
        .resolve(CoverArtTarget::Album(album))
        .await
        .unwrap()
        .expect("embedded art");

    assert!(matches!(resolved, Resolved::Local { .. }));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn artist_art_comes_from_the_first_album_with_art(pool: sqlx::PgPool) {
    let root = pictured_root("artist");
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "recording-1").await;
    let artist = repository
        .resolve_or_create_artist(&ArtistCredit::new("Artist".to_owned(), None).unwrap())
        .await
        .unwrap();
    album_with(&repository, Some("Artist"), &[track_id]).await;

    let resolved = service(repository)
        .resolve(CoverArtTarget::Artist(artist.id().clone()))
        .await
        .unwrap()
        .expect("album art");

    assert!(matches!(resolved, Resolved::Local { .. }));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn artwork_is_absent_without_pictures_or_stored_artwork(pool: sqlx::PgPool) {
    let service = service(Arc::new(PostgresTrackRepository::new(pool.clone())));

    assert!(
        service
            .resolve(CoverArtTarget::Album(AlbumId::new()))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .resolve(CoverArtTarget::Artist(ArtistId::new()))
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test]
async fn a_local_picture_is_served_with_a_stable_etag(pool: sqlx::PgPool) {
    let root = pictured_root("etag");
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "recording-1").await;
    let service = service(repository);

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Track(track_id.clone()), None, None)
        .await
        .unwrap()
    else {
        panic!("expected found artwork");
    };
    let (content_type, bytes, etag) = artwork.into_parts();
    assert_eq!(content_type, "image/jpeg");
    assert_eq!(bytes.as_ref(), &[0xFF, 0xD8, 0xFF, 0xE0]);

    assert!(matches!(
        service
            .load(CoverArtTarget::Track(track_id), Some(&etag), None)
            .await
            .unwrap(),
        ArtworkOutcome::NotModified(returned) if returned == etag
    ));

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn a_thumbnail_does_not_share_the_full_size_etag(pool: sqlx::PgPool) {
    let root = pictured_root("sized");
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let track_id = add_local_track(&repository, &root, "recording-1").await;
    let service = service(repository);

    let ArtworkOutcome::Found(artwork) = service
        .load(CoverArtTarget::Track(track_id.clone()), None, Some(300))
        .await
        .unwrap()
    else {
        panic!("expected found artwork");
    };
    let (_, _, etag) = artwork.into_parts();
    assert!(etag.ends_with("-w300\""));

    assert!(matches!(
        service
            .load(CoverArtTarget::Track(track_id), Some(&etag), Some(300))
            .await
            .unwrap(),
        ArtworkOutcome::NotModified(returned) if returned == etag
    ));

    std::fs::remove_dir_all(&root).unwrap();
}
