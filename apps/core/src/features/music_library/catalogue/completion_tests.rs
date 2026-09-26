use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;

use super::*;
use crate::{
    features::music_library::catalogue::{CatalogueCandidate, CatalogueSearch},
    features::music_library::{
        domain::{AlbumMetadata, AlbumTrack, ArtistCredit, ProviderId, SourceScope, TrackMetadata},
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
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
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
        vec![provider],
    )
    .with_registry(ProviderRegistry::with_enabled(["youtube_music"]));

    (completion, sources)
}

async fn partial_album(repository: &Arc<PostgresTrackRepository>) -> AlbumId {
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
    repository
        .set_album_source(album.id(), &provider_source("MPREb_album"))
        .await
        .unwrap();
    repository
        .set_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
        .await
        .unwrap();

    album.id().clone()
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
