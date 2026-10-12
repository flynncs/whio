// the HTTP resolver client is gone; this suite pins the DTO mapping and
// disabled-adapter behavior it used to cover instead
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    routing::{get, post},
};
use serde_json::json;
use whio_core::{
    features::music_library::catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch,
        ProviderError as CatalogueProviderError, SearchLimit, SearchQuery,
    },
    features::music_library::domain::{
        ProviderId, SourceIdentity, SourceScope, ValidationError as TrackValidationError,
    },
    features::playback::providers::ProviderError,
    features::playback::{
        PlayableMedia, PlaybackProvider, PlaybackSource, ValidationError as PlaybackValidationError,
    },
    integrations::ytm::catalogue::YtmCatalogue,
    integrations::ytm::playback::YtmPlayback,
    platform::request::{RequestContext, RequestId},
};
use whio_resolver_api::{
    apis::configuration::Configuration,
    models::{
        CatalogueAlbum as CatalogueAlbumDto, CatalogueArtist as CatalogueArtistDto,
        CatalogueCandidate as CatalogueCandidateDto, MediaMetadata as MediaMetadataDto,
        PlaybackResolveResponse as PlaybackResolveResponseDto, SourceIdentity as SourceIdentityDto,
    },
};

fn candidate_dto() -> CatalogueCandidateDto {
    CatalogueCandidateDto {
        source: Box::new(SourceIdentityDto::new(
            "example_music".to_owned(),
            "example-source-id".to_owned(),
        )),
        title: "Instant Crush".to_owned(),
        artists: vec![CatalogueArtistDto {
            name: "Daft Punk".to_owned(),
            source: Some(Box::new(SourceIdentityDto::new(
                "example_music".to_owned(),
                "artist-1".to_owned(),
            ))),
        }],
        duration_ms: Some(Some(337_000)),
        track_number: None,
        disc_number: None,
        album: Some(Box::new(CatalogueAlbumDto {
            title: "Random Access Memories".to_owned(),
            source: Some(Box::new(SourceIdentityDto::new(
                "example_music".to_owned(),
                "album-1".to_owned(),
            ))),
            year: Some(Some(2013)),
            release_type: Some(Some("Album".to_owned())),
            artwork_url: None,
        })),
        artwork_url: None,
    }
}

fn playback_dto() -> PlaybackResolveResponseDto {
    PlaybackResolveResponseDto {
        url: "https://media.example.test/audio".to_owned(),
        headers: HashMap::from([("User-Agent".to_owned(), "whio-test".to_owned())]),
        expires_at: Some(Some("2026-08-08T12:00:00+12:00".parse().unwrap())),
        media: Box::new(MediaMetadataDto {
            content_type: Some(Some("audio/webm".to_owned())),
            content_length_bytes: Some(Some(1_024)),
            codec: Some(Some("opus".to_owned())),
            bitrate_kbps: Some(Some(128.5)),
            duration_ms: Some(Some(337_250)),
        }),
    }
}

fn search() -> CatalogueSearch {
    CatalogueSearch::new(
        SearchQuery::new("Instant Crush".to_owned()).unwrap(),
        SearchLimit::new(5).unwrap(),
    )
}

fn context() -> RequestContext {
    RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
}

fn playback_source() -> PlaybackSource {
    PlaybackSource::from_identity(
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "source-123".to_owned(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn valid_candidate_dto_maps_to_domain_candidate() {
    let candidate = CatalogueCandidate::try_from(candidate_dto()).unwrap();

    assert_eq!(candidate.source().provider_id().as_str(), "example_music");
    assert_eq!(candidate.source().external_id(), "example-source-id");
    assert_eq!(candidate.title(), "Instant Crush");
    assert_eq!(
        candidate
            .artists()
            .iter()
            .map(|artist| artist.name())
            .collect::<Vec<_>>(),
        ["Daft Punk"]
    );
    assert_eq!(candidate.duration_ms(), Some(337_000));
    assert_eq!(
        candidate.artists()[0]
            .provider()
            .map(|provider| provider.external_id()),
        Some("artist-1"),
        "a provider artist identity survives the adapter"
    );
    assert_eq!(
        candidate
            .album()
            .and_then(|album| album.provider())
            .map(|provider| provider.external_id()),
        Some("album-1"),
        "a provider album identity survives the adapter"
    );
}

#[test]
fn invalid_candidate_dto_is_rejected() {
    let mut dto = candidate_dto();
    dto.duration_ms = Some(Some(-1));

    assert!(matches!(
        CatalogueCandidate::try_from(dto),
        Err(TrackValidationError::InvalidDuration)
    ));
}

#[test]
fn valid_playback_dto_maps_to_domain_media() {
    let playback = PlayableMedia::try_from(playback_dto()).unwrap();
    let (url, headers, expires_at, metadata) = playback.into_parts();

    assert_eq!(url.as_url().as_str(), "https://media.example.test/audio");
    assert_eq!(headers["User-Agent"], "whio-test");
    assert_eq!(
        expires_at.unwrap().to_rfc3339(),
        "2026-08-08T00:00:00+00:00"
    );
    assert_eq!(metadata.duration_ms(), Some(337_250));
}

#[test]
fn invalid_playback_dto_is_rejected() {
    let mut dto = playback_dto();
    dto.media.duration_ms = Some(Some(-1));

    assert!(matches!(
        PlayableMedia::try_from(dto),
        Err(PlaybackValidationError::InvalidDuration)
    ));
}

async fn resolver_server(
    capabilities: Vec<&'static str>,
    album_calls: Arc<AtomicUsize>,
    artist_search_calls: Arc<AtomicUsize>,
    artist_fetch_calls: Arc<AtomicUsize>,
) -> (String, tokio::task::JoinHandle<()>) {
    let capabilities = capabilities
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let caps_route = get(move || {
        let capabilities = capabilities.clone();
        async move {
            Json(json!({
                "contract_version": "1",
                "providers": [{
                    "provider_id": "youtube_music",
                    "capabilities": capabilities,
                }]
            }))
        }
    });
    let album_route = post(move || {
        let calls = Arc::clone(&album_calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "results": [{
                    "source": {"provider_id": "youtube_music", "external_id": "MPREb_album"},
                    "title": "SOUR",
                    "artists": [{
                        "name": "Olivia Rodrigo",
                        "source": {"provider_id": "youtube_music", "external_id": "UC-artist"}
                    }],
                    "year": 2021,
                    "release_type": "Album",
                    "artwork_url": "https://art.test/sour.jpg"
                }],
                "next_offset": 25
            }))
        }
    });
    let artist_search_route = post(move || {
        let calls = Arc::clone(&artist_search_calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "results": [{
                    "source": {"provider_id": "youtube_music", "external_id": "UC-artist"},
                    "name": "Beyoncé"
                }],
                "next_offset": null
            }))
        }
    });
    let artist_fetch_route = post(move || {
        let calls = Arc::clone(&artist_fetch_calls);
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            Json(json!({
                "results": [{
                    "source": {"provider_id": "youtube_music", "external_id": "MPREb_artist-album"},
                    "title": "SOUR",
                    "artists": [{"name": "Olivia Rodrigo", "source": {"provider_id": "youtube_music", "external_id": "UC-artist"}}],
                    "year": 2021,
                    "release_type": "Album"
                }]
            }))
        }
    });
    let app = Router::new()
        .route("/v1/capabilities", caps_route)
        .route("/v1/catalogue/album-search", album_route)
        .route("/v1/catalogue/artist-search", artist_search_route)
        .route("/v1/catalogue/artist", artist_fetch_route);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, task)
}

#[tokio::test]
async fn absent_album_search_capability_skips_the_operation_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (base, server) = resolver_server(
        vec!["catalogue.search"],
        Arc::clone(&calls),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let mut configuration = Configuration::new();
    configuration.base_path = base;
    let adapter = YtmCatalogue::new(Some(Arc::new(configuration)));

    assert!(matches!(
        adapter.search_albums(&search(), &context()).await,
        Err(CatalogueProviderError::UnsupportedProvider)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn advertised_album_search_converts_candidates_and_cursor() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (base, server) = resolver_server(
        vec!["catalogue.album-search"],
        Arc::clone(&calls),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicUsize::new(0)),
    )
    .await;
    let mut configuration = Configuration::new();
    configuration.base_path = base;
    let adapter = YtmCatalogue::new(Some(Arc::new(configuration)));

    let page = adapter.search_albums(&search(), &context()).await.unwrap();
    assert_eq!(page.items()[0].title(), "SOUR");
    assert_eq!(
        page.items()[0].provider().unwrap().external_id(),
        "MPREb_album"
    );
    assert_eq!(
        page.items()[0].album_artist().unwrap().name(),
        "Olivia Rodrigo"
    );
    assert_eq!(
        page.items()[0].artwork().map(|artwork| artwork.reference()),
        Some("https://art.test/sour.jpg")
    );
    assert_eq!(page.next_offset().unwrap().get(), 25);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn advertised_artist_operations_convert_provider_results() {
    let album_calls = Arc::new(AtomicUsize::new(0));
    let artist_search_calls = Arc::new(AtomicUsize::new(0));
    let artist_fetch_calls = Arc::new(AtomicUsize::new(0));
    let (base, server) = resolver_server(
        vec!["catalogue.artist-search", "catalogue.artist"],
        album_calls,
        Arc::clone(&artist_search_calls),
        Arc::clone(&artist_fetch_calls),
    )
    .await;
    let mut configuration = Configuration::new();
    configuration.base_path = base;
    let adapter = YtmCatalogue::new(Some(Arc::new(configuration)));

    let artists = adapter.search_artists(&search(), &context()).await.unwrap();
    assert_eq!(artists.items()[0].name(), "Beyoncé");
    assert_eq!(
        artists.items()[0].provider().unwrap().external_id(),
        "UC-artist"
    );
    let source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "UC-artist".to_owned(),
    )
    .unwrap();
    let albums = adapter
        .fetch_artist_albums(&source, &context())
        .await
        .unwrap();
    assert_eq!(albums[0].title(), "SOUR");
    assert_eq!(
        albums[0].provider().unwrap().external_id(),
        "MPREb_artist-album"
    );
    assert_eq!(artist_search_calls.load(Ordering::SeqCst), 1);
    assert_eq!(artist_fetch_calls.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn disabled_adapter_search_reports_disabled() {
    let adapter = YtmCatalogue::disabled();

    assert!(matches!(
        adapter.search_tracks(&search(), &context()).await,
        Err(CatalogueProviderError::Disabled)
    ));
    assert!(matches!(
        adapter.search_albums(&search(), &context()).await,
        Err(CatalogueProviderError::Disabled)
    ));
    assert!(matches!(
        adapter.search_artists(&search(), &context()).await,
        Err(CatalogueProviderError::Disabled)
    ));
}

#[tokio::test]
async fn disabled_adapter_resolve_reports_disabled() {
    let adapter = YtmPlayback::disabled();

    assert!(matches!(
        adapter.resolve(&playback_source(), None, &context()).await,
        Err(ProviderError::Disabled)
    ));
}

#[tokio::test]
async fn disabled_adapter_fetch_album_reports_disabled() {
    let adapter = YtmCatalogue::disabled();
    let source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "MPREb_album".to_owned(),
    )
    .unwrap();

    assert!(matches!(
        adapter.fetch_album(&source, &context()).await,
        Err(CatalogueProviderError::Disabled)
    ));
}
