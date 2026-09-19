use thiserror::Error;
use whio_subsonic_api::models;

use crate::{
    catalogue::{
        CatalogueError, CatalogueSearch, CatalogueService, SearchLimit, SearchOffset, SearchQuery,
    },
    compat::SourceTags,
    request::RequestContext,
    tracks::{AlbumId, AlbumOrder, ArtistId},
};

use super::mapping;

#[derive(Debug, Error)]
pub(crate) enum SearchError {
    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

pub(crate) struct SearchResult {
    pub(crate) songs: Vec<models::Child>,
    pub(crate) albums: Vec<models::AlbumId3>,
    pub(crate) artists: Vec<models::ArtistId3>,
}

pub(crate) async fn search(
    catalogue: &CatalogueService,
    context: &RequestContext,
    user: Option<&uuid::Uuid>,
    query: &str,
    count: Option<&str>,
    offset: Option<&str>,
    tags: SourceTags,
) -> Result<SearchResult, SearchError> {
    if query.is_empty() {
        return Ok(SearchResult {
            songs: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
        });
    }

    const DEFAULT_COUNT: u32 = 20;
    let limit = match count {
        None => DEFAULT_COUNT,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| SearchError::InvalidSearch)?,
    }
    .min(25);
    let offset = match offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| SearchError::InvalidSearch)?,
    };

    let search = match (
        SearchQuery::new(query.to_owned()),
        SearchLimit::new(limit),
        SearchOffset::new(offset),
    ) {
        (Ok(query), Ok(limit), Ok(offset)) => {
            CatalogueSearch::new_with_offset(query, limit, offset)
        }
        _ => return Err(SearchError::InvalidSearch),
    };

    catalogue
        .search(&search, user, context)
        .await
        .map(|outcome| SearchResult {
            songs: outcome
                .tracks
                .iter()
                .map(|hit| mapping::child_from_hit(hit, tags))
                .collect(),
            albums: outcome
                .albums
                .iter()
                .map(mapping::album_from_summary)
                .collect(),
            artists: outcome
                .artists
                .iter()
                .map(mapping::artist_from_summary)
                .collect(),
        })
        .map_err(SearchError::Catalogue)
}

const DEFAULT_BROWSE_SIZE: u32 = 100;
const MAX_BROWSE_SIZE: u32 = 500;

#[derive(Debug, Error)]
pub(crate) enum BrowseError {
    #[error("invalid browse parameters")]
    InvalidRequest,

    #[error("unsupported album list type")]
    UnsupportedType,

    #[error("album not found")]
    AlbumNotFound,

    #[error("artist not found")]
    ArtistNotFound,

    #[error("catalogue read failed")]
    Catalogue(#[source] CatalogueError),
}

pub(crate) async fn album_list(
    catalogue: &CatalogueService,
    list_type: &str,
    size: Option<&str>,
    offset: Option<&str>,
) -> Result<Vec<models::AlbumId3>, BrowseError> {
    let order = match list_type {
        "alphabeticalByName" => AlbumOrder::Title,
        "alphabeticalByArtist" => AlbumOrder::Artist,
        "newest" => AlbumOrder::RecentlyAdded,
        _ => return Err(BrowseError::UnsupportedType),
    };
    let size = match size {
        None => DEFAULT_BROWSE_SIZE,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| BrowseError::InvalidRequest)?,
    }
    .min(MAX_BROWSE_SIZE);
    let offset = match offset {
        None => 0,
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .map_err(|_| BrowseError::InvalidRequest)?,
    };

    catalogue
        .album_list(offset, size, order)
        .await
        .map(|albums| {
            albums
                .iter()
                .map(mapping::album_from_summary)
                .collect::<Vec<_>>()
        })
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn album_detail(
    catalogue: &CatalogueService,
    raw_id: &str,
    tags: SourceTags,
) -> Result<models::AlbumId3WithSongs, BrowseError> {
    let album_id: AlbumId = raw_id.parse().map_err(|_| BrowseError::AlbumNotFound)?;
    let detail = catalogue
        .album_detail(&album_id)
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::AlbumNotFound)?;

    Ok(mapping::album_detail_with_songs(&detail, tags))
}

pub(crate) async fn artists(
    catalogue: &CatalogueService,
) -> Result<Vec<models::ArtistId3>, BrowseError> {
    catalogue
        .artist_list(0, u32::MAX)
        .await
        .map(|artists| artists.iter().map(mapping::artist_from_summary).collect())
        .map_err(BrowseError::Catalogue)
}

pub(crate) async fn artist_detail(
    catalogue: &CatalogueService,
    raw_id: &str,
) -> Result<models::ArtistWithAlbumsId3, BrowseError> {
    let artist_id: ArtistId = raw_id.parse().map_err(|_| BrowseError::ArtistNotFound)?;
    let detail = catalogue
        .artist_detail(&artist_id)
        .await
        .map_err(BrowseError::Catalogue)?
        .ok_or(BrowseError::ArtistNotFound)?;

    Ok(mapping::artist_with_albums(&detail))
}

pub(crate) fn artist_indexes(artists: Vec<models::ArtistId3>) -> Vec<models::IndexId3> {
    let mut indexes: Vec<models::IndexId3> = Vec::new();

    for artist in artists {
        let name = index_letter(&artist.name);
        match indexes.iter_mut().find(|index| index.name == name) {
            Some(index) => index.artist.get_or_insert_with(Vec::new).push(artist),
            None => indexes.push(models::IndexId3 {
                name,
                artist: Some(vec![artist]),
            }),
        }
    }

    indexes
}

fn index_letter(name: &str) -> String {
    match name.chars().next() {
        Some(letter) if letter.is_ascii_alphabetic() => letter.to_ascii_uppercase().to_string(),
        _ => "#".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueProvider},
        providers::ProviderError,
        request::{RequestContext, RequestId},
        tracks::{
            AlbumMetadata, AlbumTrack, ArtistCredit, InMemoryTrackRepository, ProviderId,
            SourceIdentity, SourceScope, TrackMetadata, TrackRepository,
        },
    };
    use async_trait::async_trait;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };
    use uuid::Uuid;

    struct StubCatalogue {
        responses: Mutex<VecDeque<Result<Vec<CatalogueCandidate>, ProviderError>>>,
    }

    #[async_trait]
    impl CatalogueProvider for StubCatalogue {
        async fn search(
            &self,
            _search: &CatalogueSearch,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("unexpected catalogue call")
        }
    }

    fn catalogue(
        responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
    ) -> CatalogueService {
        let stub = Arc::new(StubCatalogue {
            responses: Mutex::new(responses.into()),
        });
        CatalogueService::with_providers(
            Arc::new(InMemoryTrackRepository::default()),
            vec![stub as Arc<dyn CatalogueProvider>],
        )
    }

    struct PagedCatalogue {
        total: u32,
    }

    #[async_trait]
    impl CatalogueProvider for PagedCatalogue {
        async fn search(
            &self,
            search: &CatalogueSearch,
            _user: Option<&Uuid>,
            _context: &RequestContext,
        ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
            let start = search.offset().get();
            let end = start
                .saturating_add(u32::from(search.limit().get()))
                .min(self.total);
            Ok((start..end).map(candidate_with_index).collect())
        }
    }

    fn paged_catalogue(total: u32) -> CatalogueService {
        CatalogueService::with_providers(
            Arc::new(InMemoryTrackRepository::default()),
            vec![Arc::new(PagedCatalogue { total }) as Arc<dyn CatalogueProvider>],
        )
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::generate())
    }

    fn source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).expect("valid provider"),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .expect("valid source")
    }

    fn candidate() -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .expect("valid source"),
            TrackMetadata::from_names("song".to_owned(), vec!["artist".to_owned()], Some(200_000))
                .expect("valid metadata"),
        )
    }

    fn candidate_with_index(index: u32) -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                format!("vid-{index}"),
            )
            .expect("valid source"),
            TrackMetadata::from_names(
                format!("song-{index}"),
                vec!["artist".to_owned()],
                Some(200_000),
            )
            .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn search_maps_tracks_and_short_circuits_empty() {
        let service = catalogue(vec![Ok(vec![candidate()])]);
        let result = search(
            &service,
            &context(),
            None,
            "song",
            Some("10"),
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        let songs = result.songs;
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "song");
        assert_eq!(songs[0].artist.as_deref(), Some("artist"));
        assert_eq!(songs[0].duration, Some(200));
        assert!(!songs[0].id.is_empty());

        let result = search(
            &catalogue(vec![]),
            &context(),
            None,
            "",
            None,
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert!(result.songs.is_empty());
        assert!(result.albums.is_empty());
        assert!(result.artists.is_empty());
    }

    #[tokio::test]
    async fn paging_caps_slices_and_rejects() {
        let result = search(
            &catalogue(vec![]),
            &context(),
            None,
            "song",
            Some("many"),
            None,
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SearchError::InvalidSearch)));

        let result = search(
            &catalogue(vec![]),
            &context(),
            None,
            "song",
            None,
            Some("1001"),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SearchError::InvalidSearch)));

        let service = paged_catalogue(25);
        async fn songs(
            catalogue: &CatalogueService,
            context: &RequestContext,
            count: &str,
            offset: &str,
        ) -> Vec<whio_subsonic_api::models::Child> {
            search(
                catalogue,
                context,
                None,
                "song",
                Some(count),
                Some(offset),
                SourceTags::Off,
            )
            .await
            .expect("page succeeds")
            .songs
        }
        let context = context();
        let first = songs(&service, &context, "10", "0").await;
        let second = songs(&service, &context, "10", "10").await;
        let third = songs(&service, &context, "10", "20").await;
        let bounded = songs(&service, &context, "10", "25").await;
        assert_eq!(first.len(), 10);
        assert_eq!(first[0].title, "song-0");
        assert_eq!(second[0].title, "song-10");
        assert_eq!(third.len(), 5);
        assert!(bounded.is_empty());

        let service = paged_catalogue(25);
        let songs = search(
            &service,
            &context,
            None,
            "song",
            Some("100"),
            None,
            SourceTags::Off,
        )
        .await
        .expect("search succeeds")
        .songs;
        assert_eq!(songs.len(), 25);
    }

    async fn album_catalogue(albums: &[&str]) -> CatalogueService {
        let repository = Arc::new(InMemoryTrackRepository::default());

        for title in albums {
            let metadata = AlbumMetadata::new((*title).to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Daft Punk".to_owned(), None).unwrap());
            repository.resolve_or_create_album(&metadata).await.unwrap();
        }

        CatalogueService::with_providers(repository, Vec::new())
    }

    #[tokio::test]
    async fn album_list_maps_types_and_rejects_unknown() {
        let catalogue = album_catalogue(&["Discovery", "Homework"]).await;

        let albums = album_list(&catalogue, "alphabeticalByName", None, None)
            .await
            .expect("the list succeeds");
        let names = albums
            .iter()
            .map(|album| album.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Discovery", "Homework"]);

        let recent = album_list(&catalogue, "newest", Some("1"), Some("0"))
            .await
            .expect("the list succeeds");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].name, "Homework");

        assert!(matches!(
            album_list(&catalogue, "random", None, None).await,
            Err(BrowseError::UnsupportedType)
        ));
        assert!(matches!(
            album_list(&catalogue, "alphabeticalByName", Some("many"), None).await,
            Err(BrowseError::InvalidRequest)
        ));
    }

    #[tokio::test]
    async fn album_detail_lists_its_tracks_in_order() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let album = repository
            .resolve_or_create_album(&AlbumMetadata::new("Discovery".to_owned()).unwrap())
            .await
            .unwrap();
        let second = repository
            .get_or_create(
                source("track-2"),
                TrackMetadata::from_names(
                    "Second".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(2_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let first = repository
            .get_or_create(
                source("track-1"),
                TrackMetadata::from_names(
                    "First".to_owned(),
                    vec!["Daft Punk".to_owned()],
                    Some(1_000),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_track(
                album.id(),
                second.id(),
                &AlbumTrack::new(Some(1), Some(2), 1).unwrap(),
            )
            .await
            .unwrap();
        repository
            .set_album_track(
                album.id(),
                first.id(),
                &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
            )
            .await
            .unwrap();
        let catalogue = CatalogueService::with_providers(repository, Vec::new());

        let detail = album_detail(&catalogue, &album.id().to_string(), SourceTags::Off)
            .await
            .expect("the album is found");

        assert_eq!(detail.song_count, 2);
        assert_eq!(detail.duration, 3);
        let titles = detail
            .song
            .iter()
            .map(|song| song.title.clone())
            .collect::<Vec<_>>();
        assert_eq!(titles, ["First", "Second"]);
        assert_eq!(detail.song[0].track, Some(1));
        assert_eq!(detail.song[0].disc_number, Some(1));

        assert!(matches!(
            album_detail(&catalogue, "not-a-uuid", SourceTags::Off).await,
            Err(BrowseError::AlbumNotFound)
        ));
        assert!(matches!(
            album_detail(&catalogue, &Uuid::now_v7().to_string(), SourceTags::Off).await,
            Err(BrowseError::AlbumNotFound)
        ));
    }

    #[tokio::test]
    async fn artists_group_into_indexes() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        for name in ["Daft Punk", "99 Posse"] {
            repository
                .resolve_or_create_artist(&ArtistCredit::new(name.to_owned(), None).unwrap())
                .await
                .unwrap();
        }
        let catalogue = CatalogueService::with_providers(repository, Vec::new());

        let artists = artists(&catalogue).await.expect("the list succeeds");
        let indexes = artist_indexes(artists);
        let names = indexes
            .iter()
            .map(|index| index.name.clone())
            .collect::<Vec<_>>();

        assert_eq!(names, ["#", "D"]);
        assert_eq!(indexes[1].artist.as_ref().unwrap()[0].name, "Daft Punk");
    }
}
