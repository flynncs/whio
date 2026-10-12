use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use super::*;
use crate::{
    features::music_library::catalogue::{CatalogueCandidate, CataloguePage, CatalogueSearch},
    features::music_library::{
        domain::{
            AlbumId, AlbumListFilter, AlbumMetadata, AlbumOrder, AlbumTrack, ArtistCredit,
            ArtistListFilter, ArtistSort, FullTextSelector, IdSet, ProviderId, SourceScope,
            TrackListFilter, TrackMetadata, TrackOrder,
        },
        storage::contracts::{LibraryRead, StateStore},
        storage::postgres::PostgresTrackRepository,
    },
    platform::request::RequestContext,
};

fn provider_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn album_metadata() -> AlbumMetadata {
    AlbumMetadata::new("SOUR".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("Olivia Rodrigo".to_owned(), None).unwrap())
        .with_provider(Some(provider_source("MPREb_album")))
}

fn candidate(external_id: &str, title: &str) -> CatalogueCandidate {
    let metadata = TrackMetadata::from_names(
        title.to_owned(),
        vec!["Olivia Rodrigo".to_owned()],
        Some(1_000),
    )
    .unwrap();

    CatalogueCandidate::new(provider_source(external_id), metadata)
        .with_album(Some(album_metadata()))
}

struct StubAlbumProvider {
    result: Mutex<Result<FetchedAlbum, ()>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CatalogueProvider for StubAlbumProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(Vec::new())
    }

    async fn fetch_album(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<FetchedAlbum, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);

        match self.result.lock().unwrap().clone() {
            Ok(fetched) => Ok(fetched),
            Err(()) => Err(ProviderError::ProviderUnavailable),
        }
    }
}

struct ArtistAlbumsProvider {
    album: AlbumMetadata,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl CatalogueProvider for ArtistAlbumsProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(Vec::new())
    }

    async fn fetch_artist_albums(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<Vec<AlbumMetadata>, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![self.album.clone()])
    }
}

struct PerSourceAlbumProvider {
    missing: &'static str,
    fetched: FetchedAlbum,
    requested: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl CatalogueProvider for PerSourceAlbumProvider {
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Ok(Vec::new())
    }

    async fn fetch_album(
        &self,
        source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<FetchedAlbum, ProviderError> {
        self.requested
            .lock()
            .unwrap()
            .push(source.external_id().to_owned());

        if source.external_id() == self.missing {
            return Err(ProviderError::SourceNotFound);
        }

        Ok(self.fetched.clone())
    }
}

async fn service(
    repository: &Arc<PostgresTrackRepository>,
    provider: Arc<dyn CatalogueProvider>,
) -> (CompletionService, Arc<dyn SourceLookup>) {
    let sources = Arc::clone(repository) as Arc<dyn SourceLookup>;
    let library = Arc::new(LibraryService::new(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(&sources),
        Arc::clone(repository) as Arc<dyn StateStore>,
    ));
    let completion = CompletionService::new(
        library,
        Arc::clone(repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(&sources),
        vec![Arc::clone(&provider)],
        Arc::new(SearchService::with_providers(
            Arc::clone(repository) as Arc<dyn LibraryRead>,
            Arc::clone(repository) as Arc<dyn CatalogueWrite>,
            Arc::clone(&sources),
            Arc::clone(repository) as Arc<dyn StateStore>,
            vec![provider],
        )),
    )
    .with_registry(ProviderRegistry::with_enabled(["youtube_music"]));

    (completion, sources)
}

async fn partial_album_with_sources(
    repository: &Arc<PostgresTrackRepository>,
    provider_sources: &[&str],
) -> AlbumId {
    let album = repository
        .resolve_or_create_album(&album_metadata())
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            provider_source("vid-1"),
            TrackMetadata::from_names(
                "brutal".to_owned(),
                vec!["Olivia Rodrigo".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    for source in provider_sources {
        repository
            .set_album_source(album.id(), &provider_source(source))
            .await
            .unwrap();
    }
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    album.id().clone()
}

async fn partial_album(repository: &Arc<PostgresTrackRepository>) -> AlbumId {
    partial_album_with_sources(repository, &["MPREb_album"]).await
}

#[sqlx::test]
async fn completion_stores_response_order_and_search_preserves_it(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album_id = partial_album(&repository).await;
    let songs = vec![
        candidate("vid-1", "brutal")
            .with_track_numbers(Some(1), Some(1))
            .unwrap(),
        candidate("vid-2", "z song")
            .with_track_numbers(Some(1), Some(2))
            .unwrap(),
        candidate("vid-3", "a song")
            .with_track_numbers(Some(2), Some(1))
            .unwrap(),
    ];
    let provider = Arc::new(StubAlbumProvider {
        result: Mutex::new(Ok(FetchedAlbum::new(songs, Some(3)))),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let (completion, sources) = service(&repository, provider).await;
    let detail = completion
        .album_detail(&album_id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.track().title())
            .collect::<Vec<_>>(),
        ["brutal", "z song", "a song"]
    );
    let rows: Vec<(Option<i16>, Option<i16>, i32)> = sqlx::query_as(
        "SELECT disc_number, track_number, position FROM album_tracks WHERE album_id = $1 ORDER BY position",
    ).bind(album_id.as_uuid()).fetch_all(&pool).await.unwrap();
    assert_eq!(
        rows,
        [
            (Some(1), Some(1), 0),
            (Some(1), Some(2), 1),
            (Some(2), Some(1), 2)
        ]
    );
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    super::super::materialize::materialize(
        &catalogue,
        &sources,
        vec![
            candidate("vid-2", "z song"),
            candidate("vid-1", "brutal").with_album(None),
        ],
    )
    .await
    .unwrap();
    let after: Vec<(Option<i16>, Option<i16>, i32)> = sqlx::query_as(
        "SELECT disc_number, track_number, position FROM album_tracks WHERE album_id = $1 ORDER BY position",
    ).bind(album_id.as_uuid()).fetch_all(&pool).await.unwrap();
    assert_eq!(after, rows);
}

#[sqlx::test]
async fn completed_recording_and_isrc_placements_survive_search_without_album(pool: sqlx::PgPool) {
    for (kind, numbers) in [
        ("recording", None),
        ("isrc", None),
        ("recording", Some((1, 3))),
        ("isrc", Some((1, 3))),
    ] {
        let case = format!(
            "{kind}-{}",
            if numbers.is_some() {
                "numbered"
            } else {
                "numberless"
            }
        );
        let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let mut metadata = TrackMetadata::from_names(
            format!("Strong {case}"),
            vec!["Olivia Rodrigo".to_owned()],
            Some(200_000),
        )
        .unwrap();
        metadata = if kind == "recording" {
            metadata
                .with_recording_id(Some(uuid::Uuid::now_v7().to_string()))
                .unwrap()
        } else {
            metadata
                .with_isrc(Some(
                    if numbers.is_some() {
                        "USABC2600001"
                    } else {
                        "USABC2600002"
                    }
                    .to_owned(),
                ))
                .unwrap()
        };
        let album_metadata = AlbumMetadata::new(format!("Album {case}"))
            .unwrap()
            .with_album_artist(ArtistCredit::new("Olivia Rodrigo".to_owned(), None).unwrap())
            .with_provider(Some(provider_source(&format!("MPREb_{case}"))));
        let album = repository
            .resolve_or_create_album(&album_metadata)
            .await
            .unwrap();
        repository
            .set_album_source(album.id(), album_metadata.provider().unwrap())
            .await
            .unwrap();
        let candidate =
            CatalogueCandidate::new(provider_source(&format!("video-{case}")), metadata.clone())
                .with_album(Some(album_metadata))
                .with_track_numbers(
                    numbers.map(|(disc, _)| disc),
                    numbers.map(|(_, track)| track),
                )
                .unwrap();
        let provider = Arc::new(StubAlbumProvider {
            result: Mutex::new(Ok(FetchedAlbum::new(vec![candidate], Some(1)))),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let (completion, sources) = service(&repository, provider).await;
        let detail = completion
            .album_detail(album.id(), None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.entries.len(), 1);
        let completed = detail.entries[0].membership();
        assert_eq!(completed.position(), 0);
        assert_eq!(completed.disc_number(), numbers.map(|(disc, _)| disc));
        assert_eq!(completed.track_number(), numbers.map(|(_, track)| track));
        assert_eq!(completed.disc_total(), None);
        assert_eq!(completed.track_total(), None);
        let track_id = detail.entries[0].track().id();
        let alias = repository
            .resolve_or_create_album(&AlbumMetadata::new(format!("Alias {case}")).unwrap())
            .await
            .unwrap();
        repository
            .add_album_track(
                alias.id(),
                track_id,
                &AlbumTrack::new(None, None, 2).unwrap(),
            )
            .await
            .unwrap();
        let unplaced = repository
            .resolve_or_create_album(&AlbumMetadata::new(format!("Unplaced {case}")).unwrap())
            .await
            .unwrap();
        repository
            .add_album_track(unplaced.id(), track_id, &AlbumTrack::unplaced())
            .await
            .unwrap();
        let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
        let hits = super::super::materialize::materialize(
            &catalogue,
            &sources,
            vec![CatalogueCandidate::new(
                provider_source(&format!("search-{case}")),
                metadata,
            )],
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].track().id(), track_id);
        assert!(hits[0].album().is_none());
        type MembershipRow = (
            uuid::Uuid,
            Option<i16>,
            Option<i16>,
            i32,
            Option<i16>,
            Option<i16>,
        );
        let rows: Vec<MembershipRow> = sqlx::query_as(
            "SELECT album_id, disc_number, track_number, position, disc_total, track_total FROM album_tracks WHERE track_id = $1 ORDER BY album_id",
        ).bind(track_id.as_uuid()).fetch_all(&pool).await.unwrap();
        assert_eq!(
            rows,
            [
                (
                    *album.id().as_uuid(),
                    numbers.map(|(disc, _)| i16::try_from(disc).unwrap()),
                    numbers.map(|(_, track)| i16::try_from(track).unwrap()),
                    0,
                    None,
                    None
                ),
                (*alias.id().as_uuid(), None, None, 2, None, None),
                (*unplaced.id().as_uuid(), None, None, 0, None, None),
            ]
        );
    }
}

#[sqlx::test]
async fn artist_detail_completes_each_provider_discography_once(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artist_source = provider_source("UC-artist");
    let artist = repository
        .resolve_or_create_artist(
            &ArtistCredit::new("Olivia Rodrigo".to_owned(), None)
                .unwrap()
                .with_provider(Some(artist_source.clone())),
        )
        .await
        .unwrap();
    repository
        .set_artist_source(artist.id(), &artist_source)
        .await
        .unwrap();
    let album = AlbumMetadata::new("SOUR".to_owned())
        .unwrap()
        .with_provider(Some(provider_source("MPREb_artist-album")))
        .with_album_artist(
            ArtistCredit::new("Olivia Rodrigo".to_owned(), None)
                .unwrap()
                .with_provider(Some(artist_source.clone())),
        );
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let library = Arc::new(LibraryService::new(
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
        Arc::clone(&sources),
        Arc::clone(&repository) as Arc<dyn StateStore>,
    ));
    let calls = Arc::new(AtomicUsize::new(0));
    let completion = CompletionService::new(
        library,
        Arc::clone(&repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(&sources),
        vec![Arc::new(ArtistAlbumsProvider {
            album,
            calls: Arc::clone(&calls),
        })],
        Arc::new(SearchService::new(
            Arc::clone(&repository) as Arc<dyn LibraryRead>,
            Arc::clone(&repository) as Arc<dyn CatalogueWrite>,
            Arc::clone(&sources),
            Arc::clone(&repository) as Arc<dyn StateStore>,
        )),
    );

    let detail = completion
        .artist_detail(artist.id(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(detail.albums.len(), 1);
    assert_eq!(detail.albums[0].album().title(), "SOUR");
    assert_eq!(detail.albums[0].track_count(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let filter = AlbumListFilter::new().with_artist_id(Some(
        crate::features::music_library::domain::IdSet::new([artist.id().clone()]).unwrap(),
    ));
    let page = completion
        .album_page(
            0,
            crate::features::music_library::domain::PageLimit::new(1).unwrap(),
            crate::features::music_library::domain::AlbumOrder::Name,
            &filter,
            None,
            &RequestContext::new(crate::platform::request::RequestId::generate()),
        )
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.albums[0].album().title(), "SOUR");

    completion.artist_detail(artist.id(), None).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let state = sources
        .find_provider_entity_state(&artist_source)
        .await
        .unwrap()
        .unwrap();
    assert!(state.last_error().is_none());
}

#[sqlx::test]
async fn opening_a_partial_provider_album_completes_it_once(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album_id = partial_album(&repository).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(StubAlbumProvider {
        result: Mutex::new(Ok(FetchedAlbum::new(
            vec![
                candidate("vid-2", "traitor"),
                candidate("vid-3", "drivers license"),
            ],
            Some(3),
        ))),
        calls: Arc::clone(&calls),
    });
    let (completion, sources) = service(&repository, provider).await;

    let detail = completion
        .album_detail(&album_id, None)
        .await
        .unwrap()
        .expect("the album is found");

    assert_eq!(detail.entries.len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let state = sources
        .find_provider_entity_state(&provider_source("MPREb_album"))
        .await
        .unwrap()
        .expect("completion records its freshness");
    assert_eq!(state.reported_count(), Some(3));
    assert!(!state.is_partial(3));

    completion.album_detail(&album_id, None).await.unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a settled album is not fetched again"
    );
}

#[sqlx::test]
async fn an_unavailable_provider_degrades_to_the_partial_album(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album_id = partial_album(&repository).await;
    let provider = Arc::new(StubAlbumProvider {
        result: Mutex::new(Err(())),
        calls: Arc::new(AtomicUsize::new(0)),
    });
    let (completion, sources) = service(&repository, provider).await;

    let detail = completion
        .album_detail(&album_id, None)
        .await
        .unwrap()
        .expect("the album is still found");

    assert_eq!(detail.entries.len(), 1);
    let state = sources
        .find_provider_entity_state(&provider_source("MPREb_album"))
        .await
        .unwrap()
        .expect("a failure is recorded");
    assert_eq!(state.reported_count(), None);
    assert!(state.last_error().is_some());
}

#[sqlx::test]
async fn an_owned_album_never_asks_the_provider(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album = repository
        .resolve_or_create_album(&album_metadata())
        .await
        .unwrap();
    let local = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "path-a".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(
            local,
            TrackMetadata::from_names(
                "brutal".to_owned(),
                vec!["Olivia Rodrigo".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &provider_source("MPREb_album"))
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(StubAlbumProvider {
        result: Mutex::new(Ok(FetchedAlbum::new(Vec::new(), None))),
        calls: Arc::clone(&calls),
    });
    let (completion, _) = service(&repository, provider).await;

    let detail = completion
        .album_detail(album.id(), None)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(detail.entries.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[sqlx::test]
async fn an_unusable_source_falls_through_to_a_working_one(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album_id = partial_album_with_sources(&repository, &["30ce80xVHL4", "MPREb_album"]).await;
    let requested = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(PerSourceAlbumProvider {
        missing: "30ce80xVHL4",
        fetched: FetchedAlbum::new(
            vec![
                candidate("vid-2", "traitor"),
                candidate("vid-3", "drivers license"),
            ],
            Some(3),
        ),
        requested: Arc::clone(&requested),
    });
    let (completion, sources) = service(&repository, provider).await;

    let detail = completion
        .album_detail(&album_id, None)
        .await
        .unwrap()
        .expect("the album is found");

    assert_eq!(detail.entries.len(), 3);
    assert_eq!(
        requested.lock().unwrap().as_slice(),
        ["30ce80xVHL4", "MPREb_album"],
        "the unusable source is skipped and the working one completes the album"
    );

    let state = sources
        .find_provider_entity_state(&provider_source("MPREb_album"))
        .await
        .unwrap()
        .expect("the working source records its freshness");
    assert_eq!(state.reported_count(), Some(3));
    assert!(state.last_error().is_none());

    assert!(
        sources
            .find_provider_entity_state(&provider_source("30ce80xVHL4"))
            .await
            .unwrap()
            .is_none(),
        "a source that cannot identify the album is not recorded as a failure"
    );
}

#[sqlx::test]
async fn an_album_with_only_an_unusable_source_degrades_to_the_partial_album(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let album_id = partial_album_with_sources(&repository, &["30ce80xVHL4"]).await;
    let provider = Arc::new(PerSourceAlbumProvider {
        missing: "30ce80xVHL4",
        fetched: FetchedAlbum::new(Vec::new(), None),
        requested: Arc::new(Mutex::new(Vec::new())),
    });
    let (completion, sources) = service(&repository, provider).await;

    let detail = completion
        .album_detail(&album_id, None)
        .await
        .unwrap()
        .expect("the album is still found");

    assert_eq!(detail.entries.len(), 1);
    let state = sources
        .find_provider_entity_state(&provider_source("30ce80xVHL4"))
        .await
        .unwrap()
        .expect("a failure is recorded when every source is unusable");
    assert!(state.last_error().is_some());
}

#[derive(Clone, Copy)]
enum ListSearchOutcome {
    Results,
    Access,
    Unavailable,
}

struct ListSearchProvider {
    outcome: ListSearchOutcome,
    calls: Arc<Mutex<Vec<String>>>,
}

impl ListSearchProvider {
    fn record(&self, kind: &str, search: &CatalogueSearch) {
        self.calls.lock().unwrap().push(format!(
            "{kind}:{}:{}",
            search.query().as_str(),
            u32::from(search.limit().get()) + search.offset().get()
        ));
    }

    fn failure(&self) -> ProviderError {
        match self.outcome {
            ListSearchOutcome::Access => ProviderError::Access,
            ListSearchOutcome::Unavailable => ProviderError::ProviderUnavailable,
            ListSearchOutcome::Results => ProviderError::UnsupportedProvider,
        }
    }

    fn artist(&self) -> ArtistCredit {
        ArtistCredit::new("beabadoobee".to_owned(), None)
            .unwrap()
            .with_provider(Some(provider_source("artist-beabadoobee")))
    }

    fn album(&self) -> AlbumMetadata {
        AlbumMetadata::new("Beatopia".to_owned())
            .unwrap()
            .with_provider(Some(provider_source("album-beatopia")))
            .with_album_artist(self.artist())
    }
}

#[async_trait]
impl CatalogueProvider for ListSearchProvider {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        self.record("tracks", search);
        match self.outcome {
            ListSearchOutcome::Results => Ok(vec![candidate("listed-track", "Glue Song")]),
            _ => Err(self.failure()),
        }
    }

    async fn search_albums(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<AlbumMetadata>, ProviderError> {
        self.record("albums", search);
        match self.outcome {
            ListSearchOutcome::Results => Ok(CataloguePage::new(vec![self.album()], None)),
            _ => Err(self.failure()),
        }
    }

    async fn search_artists(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<ArtistCredit>, ProviderError> {
        self.record("artists", search);
        match self.outcome {
            ListSearchOutcome::Results => Ok(CataloguePage::new(vec![self.artist()], None)),
            _ => Err(self.failure()),
        }
    }
}

fn list_completion(
    repository: &Arc<PostgresTrackRepository>,
    provider: Arc<dyn CatalogueProvider>,
) -> CompletionService {
    let sources = Arc::clone(repository) as Arc<dyn SourceLookup>;
    let library = Arc::new(LibraryService::new(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(&sources),
        Arc::clone(repository) as Arc<dyn StateStore>,
    ));
    let search = Arc::new(SearchService::with_providers(
        Arc::clone(repository) as Arc<dyn LibraryRead>,
        Arc::clone(repository) as Arc<dyn CatalogueWrite>,
        Arc::clone(&sources),
        Arc::clone(repository) as Arc<dyn StateStore>,
        vec![Arc::clone(&provider)],
    ));
    CompletionService::new(
        library,
        Arc::clone(repository) as Arc<dyn CatalogueWrite>,
        sources,
        vec![provider],
        search,
    )
}

fn selector(text: &str) -> Option<FullTextSelector> {
    FullTextSelector::new(text).unwrap()
}

#[sqlx::test]
async fn text_list_pages_discover_before_reading_and_widen_each_scope(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(ListSearchProvider {
        outcome: ListSearchOutcome::Results,
        calls: Arc::clone(&calls),
    });
    let completion = list_completion(&repository, provider);
    let context = RequestContext::new(crate::platform::request::RequestId::generate());
    let limit = PageLimit::new(10).unwrap();

    let empty_album_page = completion
        .album_page(
            0,
            limit,
            AlbumOrder::Name,
            &AlbumListFilter::new(),
            None,
            &context,
        )
        .await
        .unwrap();
    assert_eq!(empty_album_page.total, 0);
    let empty_artist_page = completion
        .artist_page(
            0,
            limit,
            ArtistSort::Name,
            &ArtistListFilter::new(),
            None,
            &context,
        )
        .await
        .unwrap();
    assert_eq!(empty_artist_page.total, 0);
    let empty_track_page = completion
        .track_page(
            0,
            limit,
            TrackOrder::Title,
            &TrackListFilter::new(),
            None,
            &context,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty_track_page.total(), 0);
    assert!(calls.lock().unwrap().is_empty());

    let album_filter = AlbumListFilter::new().with_name(selector("beabadoobee"));
    let albums = completion
        .album_page(0, limit, AlbumOrder::Name, &album_filter, None, &context)
        .await
        .unwrap();
    assert_eq!(albums.total, 1);
    assert_eq!(albums.albums[0].album().title(), "Beatopia");

    let artist_filter = ArtistListFilter::new().with_name(selector("bea"));
    let artist_page = completion
        .artist_page(0, limit, ArtistSort::Name, &artist_filter, None, &context)
        .await
        .unwrap();
    assert_eq!(artist_page.total, 1);
    assert_eq!(artist_page.artists[0].artist().name(), "beabadoobee");

    let track_filter = TrackListFilter::new().with_title(selector("glue"));
    let tracks = completion
        .track_page(0, limit, TrackOrder::Title, &track_filter, None, &context)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tracks.total(), 1);
    assert_eq!(tracks.items()[0].track().title(), "Glue Song");
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["albums:beabadoobee:10", "artists:bea:10", "tracks:glue:10"]
    );
}

#[sqlx::test]
async fn a_list_request_discovers_one_provider_page(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let completion = list_completion(
        &repository,
        Arc::new(ListSearchProvider {
            outcome: ListSearchOutcome::Results,
            calls: Arc::clone(&calls),
        }),
    );
    let context = RequestContext::new(crate::platform::request::RequestId::generate());
    let limit = PageLimit::new(100).unwrap();

    completion
        .album_page(
            0,
            limit,
            AlbumOrder::Name,
            &AlbumListFilter::new().with_name(selector("beabadoobee")),
            None,
            &context,
        )
        .await
        .unwrap();
    completion
        .artist_page(
            0,
            limit,
            ArtistSort::Name,
            &ArtistListFilter::new().with_name(selector("bea")),
            None,
            &context,
        )
        .await
        .unwrap();
    completion
        .track_page(
            0,
            limit,
            TrackOrder::Title,
            &TrackListFilter::new().with_title(selector("glue")),
            None,
            &context,
        )
        .await
        .unwrap();

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["albums:beabadoobee:25", "artists:bea:25", "tracks:glue:25"]
    );
}

struct FullPageTrackProvider {
    offsets: Arc<Mutex<Vec<u32>>>,
}

#[async_trait]
impl CatalogueProvider for FullPageTrackProvider {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let offset = search.offset().get();
        self.offsets.lock().unwrap().push(offset);
        Ok((offset..offset + u32::from(search.limit().get()))
            .map(|index| {
                CatalogueCandidate::new(
                    provider_source(&format!("deep-track-{index}")),
                    TrackMetadata::from_names(
                        "Needle Track".to_owned(),
                        vec!["Artist".to_owned()],
                        Some(1_000),
                    )
                    .unwrap(),
                )
            })
            .collect())
    }
}

#[sqlx::test]
async fn deep_text_track_page_discovers_one_provider_page(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let offsets = Arc::new(Mutex::new(Vec::new()));
    let completion = list_completion(
        &repository,
        Arc::new(FullPageTrackProvider {
            offsets: Arc::clone(&offsets),
        }),
    );
    let filter = TrackListFilter::new().with_title(selector("needle"));
    let context = RequestContext::new(crate::platform::request::RequestId::generate());

    let page = completion
        .track_page(
            1_001,
            PageLimit::new(25).unwrap(),
            TrackOrder::Title,
            &filter,
            None,
            &context,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(page.total(), 25);
    assert!(page.items().is_empty());
    assert_eq!(offsets.lock().unwrap().as_slice(), [0]);
}

#[sqlx::test]
async fn text_track_discovery_access_fails_for_an_unknown_album(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let completion = list_completion(
        &repository,
        Arc::new(ListSearchProvider {
            outcome: ListSearchOutcome::Access,
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    let filter = TrackListFilter::new()
        .with_title(selector("missing"))
        .with_album_id(Some(IdSet::new([AlbumId::new()]).unwrap()));
    let context = RequestContext::new(crate::platform::request::RequestId::generate());

    let result = completion
        .track_page(
            0,
            PageLimit::new(10).unwrap(),
            TrackOrder::Title,
            &filter,
            None,
            &context,
        )
        .await;

    assert!(matches!(
        result,
        Err(CatalogueError::Provider(ProviderError::Access))
    ));
}

#[sqlx::test]
async fn padded_maximum_selector_is_discoverable_and_keeps_its_raw_query(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let completion = list_completion(
        &repository,
        Arc::new(ListSearchProvider {
            outcome: ListSearchOutcome::Results,
            calls: Arc::clone(&calls),
        }),
    );
    let query = format!(" {} ", "x".repeat(500));
    let selector = FullTextSelector::new(&query).unwrap().unwrap();
    assert!(FullTextSelector::new(&format!(" {} ", "x".repeat(501))).is_err());
    let filter = TrackListFilter::new().with_title(Some(selector));
    let context = RequestContext::new(crate::platform::request::RequestId::generate());

    let page = completion
        .track_page(
            0,
            PageLimit::new(10).unwrap(),
            TrackOrder::Title,
            &filter,
            None,
            &context,
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(page.total(), 0);
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        [format!("tracks:{query}:10")]
    );
}

#[sqlx::test]
async fn text_album_discovery_errors_follow_search3_rules(pool: sqlx::PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let local_source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "local-track".to_owned(),
    )
    .unwrap();
    let album_source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "local-album".to_owned(),
    )
    .unwrap();
    let album_metadata = AlbumMetadata::new("Local Match".to_owned())
        .unwrap()
        .with_provider(Some(album_source.clone()))
        .with_album_artist(ArtistCredit::new("beabadoobee".to_owned(), None).unwrap());
    let album = repository
        .resolve_or_create_album(&album_metadata)
        .await
        .unwrap();
    repository
        .set_album_source(album.id(), &album_source)
        .await
        .unwrap();
    let track = repository
        .get_or_create(
            local_source,
            TrackMetadata::from_names(
                "Local Track".to_owned(),
                vec!["beabadoobee".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    let access = list_completion(
        &repository,
        Arc::new(ListSearchProvider {
            outcome: ListSearchOutcome::Access,
            calls: Arc::clone(&calls),
        }),
    );
    let filter = AlbumListFilter::new().with_name(selector("beabadoobee"));
    let context = RequestContext::new(crate::platform::request::RequestId::generate());
    let error = access
        .album_page(
            0,
            PageLimit::new(10).unwrap(),
            AlbumOrder::Name,
            &filter,
            None,
            &context,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CatalogueError::Provider(ProviderError::Access)
    ));

    let unavailable = list_completion(
        &repository,
        Arc::new(ListSearchProvider {
            outcome: ListSearchOutcome::Unavailable,
            calls: Arc::clone(&calls),
        }),
    );
    let local = unavailable
        .album_page(
            0,
            PageLimit::new(10).unwrap(),
            AlbumOrder::Name,
            &filter,
            None,
            &context,
        )
        .await
        .unwrap();
    assert_eq!(local.total, 1);
    assert_eq!(local.albums[0].album().title(), "Local Match");

    let missing = AlbumListFilter::new().with_name(selector("absent"));
    let error = unavailable
        .album_page(
            0,
            PageLimit::new(10).unwrap(),
            AlbumOrder::Name,
            &missing,
            None,
            &context,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CatalogueError::Provider(ProviderError::ProviderUnavailable)
    ));
}

#[sqlx::test]
async fn opening_a_provider_single_whose_track_matches_the_library_lists_one_entry(
    pool: sqlx::PgPool,
) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let library_track = repository
        .get_or_create(
            SourceIdentity::new(
                ProviderId::new("local".to_owned()).unwrap(),
                SourceScope::Named("/music".to_owned()),
                "care.flac".to_owned(),
            )
            .unwrap(),
            TrackMetadata::from_names(
                "Care".to_owned(),
                vec!["beabadoobee".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let library_album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Fake It Flowers".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("beabadoobee".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            library_album.id(),
            library_track.id(),
            &AlbumTrack::unplaced(),
        )
        .await
        .unwrap();

    let single_metadata = AlbumMetadata::new("Care".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("beabadoobee".to_owned(), None).unwrap())
        .with_provider(Some(provider_source("MPREb_single")));
    let single = repository
        .resolve_or_create_album(&single_metadata)
        .await
        .unwrap();
    repository
        .set_album_source(single.id(), &provider_source("MPREb_single"))
        .await
        .unwrap();

    let candidate = CatalogueCandidate::new(
        provider_source("video-care"),
        TrackMetadata::from_names(
            "Care".to_owned(),
            vec!["beabadoobee".to_owned()],
            Some(1_000),
        )
        .unwrap(),
    )
    .with_album(Some(single_metadata));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(StubAlbumProvider {
        result: Mutex::new(Ok(FetchedAlbum::new(vec![candidate], Some(1)))),
        calls: Arc::clone(&calls),
    });
    let (completion, sources) = service(&repository, provider).await;

    let detail = completion
        .album_detail(single.id(), None)
        .await
        .unwrap()
        .expect("the single is found");
    assert_eq!(detail.entries.len(), 1);
    assert_eq!(detail.entries[0].track().id(), library_track.id());
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let state = sources
        .find_provider_entity_state(&provider_source("MPREb_single"))
        .await
        .unwrap()
        .expect("completion records its freshness");
    assert_eq!(state.reported_count(), Some(1));
    assert!(!state.is_partial(1));

    let again = completion
        .album_detail(single.id(), None)
        .await
        .unwrap()
        .expect("the single is found");
    assert_eq!(again.entries.len(), 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a settled single is not fetched again"
    );
}
