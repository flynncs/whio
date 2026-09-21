use thiserror::Error;
use uuid::Uuid;

use crate::{
    artwork::CoverArtTarget,
    catalogue::{
        CatalogueError, CatalogueSearch, LibraryService, SearchHit, SearchLimit, SearchOffset,
        SearchQuery, SearchService,
    },
    compat::SourceTags,
    identity::service::{AuthError, CredentialService},
    request::RequestContext,
    tracks::{
        AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, ArtistId, ArtistSummary,
        GenreSummary, TrackId,
    },
};

use super::{
    dto::{Album, Artist, Genre, Song, SongListQuery},
    session,
};

#[derive(Debug, Error)]
pub(crate) enum SongError {
    #[error("not authenticated")]
    Unauthorized,

    #[error("authentication storage failed")]
    Authentication(#[source] AuthError),

    #[error("track not found")]
    NotFound,

    #[error("invalid search parameters")]
    InvalidSearch,

    #[error("catalogue search failed")]
    Catalogue(#[source] CatalogueError),
}

fn song_from(hit: &SearchHit, tags: SourceTags) -> Song {
    let track = hit.track();
    let album = hit.album();
    let state = hit.state();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), hit.sources()),
        album: album.map(|album| album.title().to_owned()),
        album_id: album.map(|album| album.id().to_string()),
        album_artist: None,
        track_number: None,
        disc_number: None,
        cover_art: Some(CoverArtTarget::song(track.id()).encode()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn song_from_entry(entry: &AlbumEntry, album: &AlbumSummary, tags: SourceTags) -> Song {
    let track = entry.track();
    let membership = entry.membership();
    let state = entry.state();

    Song {
        id: track.id().to_string(),
        title: track.title().to_owned(),
        artist: tags.artist(track.artists(), entry.sources()),
        album: Some(album.album().title().to_owned()),
        album_id: Some(album.album().id().to_string()),
        album_artist: album.album_artist().map(|artist| artist.name().to_owned()),
        track_number: membership.track_number().map(i32::from),
        disc_number: membership.disc_number().map(i32::from),
        cover_art: Some(track.id().to_string()),
        duration: track
            .duration_ms()
            .map(|ms| ms as f32 / 1000.0)
            .unwrap_or(0.0),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn album_from_summary(summary: &AlbumSummary) -> Album {
    let album = summary.album();
    let artist = summary.album_artist();
    let created_at = summary.created_at().to_rfc3339();
    let state = summary.state();

    Album {
        id: album.id().to_string(),
        name: album.title().to_owned(),
        album_artist: artist.map_or_else(String::new, |artist| artist.name().to_owned()),
        album_artist_id: artist.map_or_else(String::new, |artist| artist.id().to_string()),
        song_count: summary.track_count(),
        duration: summary.duration_ms() as f32 / 1000.0,
        size: 0,
        cover_art: Some(CoverArtTarget::album(album.id()).encode()),
        year: album.year(),
        min_year: album.year().unwrap_or(0),
        max_year: album.year().unwrap_or(0),
        compilation: false,
        created_at: created_at.clone(),
        updated_at: created_at,
        genres: None,
        library_id: 1,
        library_name: "whio".to_owned(),
        library_path: String::new(),
        order_album_name: album.title().to_lowercase(),
        order_album_artist_name: artist
            .map_or_else(String::new, |artist| artist.name().to_lowercase()),
        external_info_updated_at: None,
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn artist_from_summary(summary: &ArtistSummary) -> Artist {
    let artist = summary.artist();
    let state = summary.state();

    Artist {
        id: artist.id().to_string(),
        name: artist.name().to_owned(),
        album_count: summary.album_count(),
        song_count: 0,
        size: 0,
        missing: false,
        cover_art: Some(CoverArtTarget::artist(artist.id()).encode()),
        sort_artist_name: artist
            .sort_name()
            .map_or_else(|| artist.name().to_lowercase(), str::to_owned),
        order_artist_name: artist.name().to_lowercase(),
        mbz_artist_id: artist.musicbrainz_artist_id().map(str::to_owned),
        starred: state.is_starred(),
        starred_at: state.starred_at().map(|at| at.to_rfc3339()),
        rating: state.rating().unwrap_or(0),
        play_count: u64::from(state.play_count()),
        play_date: state.last_played_at().map(|at| at.to_rfc3339()),
    }
}

fn genre_from_summary(summary: &GenreSummary) -> Genre {
    Genre {
        id: summary.genre().to_owned(),
        name: summary.genre().to_owned(),
        song_count: u64::from(summary.song_count()),
        album_count: u64::from(summary.album_count()),
    }
}

pub(crate) async fn search_songs(
    search_service: &SearchService,
    credential: &CredentialService,
    context: &RequestContext,
    token: &str,
    query: &SongListQuery,
    tags: SourceTags,
) -> Result<Vec<Song>, SongError> {
    let user_id = match session::authorize(credential, token).await {
        Ok(Some(user_id)) => user_id,
        Ok(None) => return Err(SongError::Unauthorized),
        Err(error) => return Err(SongError::Authentication(error)),
    };
    let user_uuid = user_id.as_uuid();

    let q = query.title.as_deref().unwrap_or("").trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let start = query.start.unwrap_or(0);
    let end = match query.end {
        Some(end) if end >= 0 => end as u32,
        _ => start.saturating_add(24),
    };
    if end < start {
        return Err(SongError::InvalidSearch);
    }
    let limit = end.saturating_sub(start).saturating_add(1).clamp(1, 25);
    let search = match (
        SearchQuery::new(q.to_owned()),
        SearchLimit::new(limit),
        SearchOffset::new(start),
    ) {
        (Ok(query), Ok(limit), Ok(offset)) => {
            CatalogueSearch::new_with_offset(query, limit, offset)
        }
        _ => return Err(SongError::InvalidSearch),
    };

    let outcome = search_service
        .search(&search, Some(&user_uuid), context)
        .await
        .map_err(SongError::Catalogue)?;

    Ok(outcome
        .tracks
        .iter()
        .map(|hit| song_from(hit, tags))
        .collect())
}

pub(crate) async fn song_detail(
    library: &LibraryService,
    credential: &CredentialService,
    raw_id: &str,
    token: &str,
    tags: SourceTags,
) -> Result<Song, SongError> {
    let user = authorize(credential, token).await?;

    let track_id: TrackId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let hit = library
        .track_detail(&track_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(song_from(&hit, tags))
}

const DEFAULT_PAGE_SIZE: u32 = 500;

pub(crate) async fn album_songs(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    raw_album_id: &str,
    tags: SourceTags,
) -> Result<Vec<Song>, SongError> {
    let user = authorize(credential, token).await?;
    let album_id: AlbumId = raw_album_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = library
        .album_detail(&album_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(detail
        .entries
        .iter()
        .map(|entry| song_from_entry(entry, &detail.album, tags))
        .collect())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn album_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    order: AlbumOrder,
    filter: AlbumListFilter,
    artist_id: Option<&str>,
    start: u32,
    end: u32,
) -> Result<(Vec<Album>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end);

    match artist_id {
        Some(raw) => {
            let artist_id: ArtistId = raw.parse().map_err(|_| SongError::InvalidSearch)?;
            let detail = library
                .artist_detail(&artist_id, Some(&user))
                .await
                .map_err(SongError::Catalogue)?
                .ok_or(SongError::NotFound)?;
            let total = detail.albums.len() as u32;
            let albums = detail
                .albums
                .iter()
                .skip(offset as usize)
                .take(limit as usize)
                .map(album_from_summary)
                .collect();

            Ok((albums, total))
        }
        None => {
            let total = library
                .album_count(Some(&user), filter.clone())
                .await
                .map_err(SongError::Catalogue)?;
            let albums = library
                .album_list(offset, limit, order, filter, Some(&user))
                .await
                .map_err(SongError::Catalogue)?;

            Ok((albums.iter().map(album_from_summary).collect(), total))
        }
    }
}

pub(crate) async fn album_detail(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<Album, SongError> {
    let user = authorize(credential, token).await?;
    let album_id: AlbumId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = library
        .album_detail(&album_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(album_from_summary(&detail.album))
}

pub(crate) async fn artist_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    start: u32,
    end: u32,
) -> Result<(Vec<Artist>, u32), SongError> {
    let user = authorize(credential, token).await?;
    let (offset, limit) = page(start, end);
    let total = library
        .artist_count(Some(&user))
        .await
        .map_err(SongError::Catalogue)?;
    let artists = library
        .artist_list(offset, limit, Some(&user))
        .await
        .map_err(SongError::Catalogue)?;

    Ok((artists.iter().map(artist_from_summary).collect(), total))
}

pub(crate) async fn artist_detail(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
    raw_id: &str,
) -> Result<Artist, SongError> {
    let user = authorize(credential, token).await?;
    let artist_id: ArtistId = raw_id.parse().map_err(|_| SongError::NotFound)?;
    let detail = library
        .artist_detail(&artist_id, Some(&user))
        .await
        .map_err(SongError::Catalogue)?
        .ok_or(SongError::NotFound)?;

    Ok(artist_from_summary(&detail.artist))
}

pub(crate) fn album_order(sort: Option<&str>) -> AlbumOrder {
    let sort = sort.unwrap_or_default().to_ascii_lowercase();
    let sort = sort.replace('_', "");

    match sort.as_str() {
        "albumartist" | "artist" => AlbumOrder::Artist,
        "recentlyadded" | "createdat" => AlbumOrder::RecentlyAdded,
        "year" => AlbumOrder::YearAscending,
        "playcount" => AlbumOrder::Frequent,
        "playdate" | "lastplayed" => AlbumOrder::Recent,
        "rating" => AlbumOrder::HighestRated,
        "starredat" | "starred" => AlbumOrder::Starred,
        _ => AlbumOrder::Title,
    }
}

pub(crate) async fn genre_list(
    library: &LibraryService,
    credential: &CredentialService,
    token: &str,
) -> Result<Vec<Genre>, SongError> {
    authorize(credential, token).await?;

    library
        .genres()
        .await
        .map(|genres| genres.iter().map(genre_from_summary).collect())
        .map_err(SongError::Catalogue)
}

async fn authorize(credential: &CredentialService, token: &str) -> Result<Uuid, SongError> {
    match session::authorize(credential, token).await {
        Ok(Some(user_id)) => Ok(user_id.as_uuid()),
        Ok(None) => Err(SongError::Unauthorized),
        Err(error) => Err(SongError::Authentication(error)),
    }
}

fn page(start: u32, end: u32) -> (u32, u32) {
    if end > start {
        (start, end - start)
    } else {
        (start, DEFAULT_PAGE_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{domain::UserId, secrets::CredentialKey};
    use crate::tracks::{CatalogueWrite, LibraryRead, SourceLookup, StateStore};
    use crate::{
        catalogue::{CatalogueCandidate, CatalogueProvider},
        providers::ProviderError,
        request::{RequestContext, RequestId},
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, Track, TrackId,
            TrackMetadata,
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

    fn key() -> CredentialKey {
        CredentialKey::from_base64("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .expect("valid key")
    }

    struct StubCredentialStore {
        user: UserId,
        credential: Uuid,
    }

    #[async_trait]
    impl crate::identity::repository::CredentialRepository for StubCredentialStore {
        async fn find_user_by_username(
            &self,
            _username: &str,
        ) -> Result<
            Option<crate::identity::domain::User>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(None)
        }

        async fn find_active_api_key_by_digest(
            &self,
            _digest: [u8; 32],
        ) -> Result<
            Option<crate::identity::domain::ApiKeyCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(None)
        }

        async fn list_active_app_passwords(
            &self,
            _user_id: UserId,
        ) -> Result<
            Vec<crate::identity::domain::AppPasswordCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok(Vec::new())
        }

        async fn find_active_app_password(
            &self,
            credential_id: Uuid,
        ) -> Result<
            Option<crate::identity::domain::AppPasswordCredential>,
            crate::identity::repository::CredentialStoreError,
        > {
            Ok((credential_id == self.credential).then(|| {
                crate::identity::domain::AppPasswordCredential {
                    id: self.credential,
                    user_id: self.user,
                    encrypted_secret: Vec::new(),
                }
            }))
        }
    }

    struct TestAuth {
        service: std::sync::Arc<CredentialService>,
        token: String,
    }

    fn auth() -> TestAuth {
        let key = key();
        let user = UserId::from_uuid(Uuid::now_v7());
        let credential = Uuid::now_v7();
        let service = std::sync::Arc::new(CredentialService::new(
            std::sync::Arc::new(StubCredentialStore { user, credential }),
            key.clone(),
        ));
        let token = session::mint(user, credential, &key);
        TestAuth { service, token }
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::generate())
    }

    fn services(
        responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>,
    ) -> (SearchService, LibraryService) {
        let stub = Arc::new(StubCatalogue {
            responses: Mutex::new(responses.into()),
        });
        let store = Arc::new(InMemoryTrackRepository::default());
        let search = SearchService::with_providers(
            Arc::clone(&store) as Arc<dyn LibraryRead>,
            Arc::clone(&store) as Arc<dyn CatalogueWrite>,
            Arc::clone(&store) as Arc<dyn SourceLookup>,
            Arc::clone(&store) as Arc<dyn StateStore>,
            vec![stub as Arc<dyn CatalogueProvider>],
        );
        let library = LibraryService::new(
            Arc::clone(&store) as Arc<dyn LibraryRead>,
            Arc::clone(&store) as Arc<dyn SourceLookup>,
            Arc::clone(&store) as Arc<dyn StateStore>,
        );
        (search, library)
    }

    fn catalogue(responses: Vec<Result<Vec<CatalogueCandidate>, ProviderError>>) -> SearchService {
        services(responses).0
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

    fn paged_catalogue(total: u32) -> SearchService {
        let store = Arc::new(InMemoryTrackRepository::default());
        SearchService::with_providers(
            Arc::clone(&store) as Arc<dyn LibraryRead>,
            Arc::clone(&store) as Arc<dyn CatalogueWrite>,
            Arc::clone(&store) as Arc<dyn SourceLookup>,
            Arc::clone(&store) as Arc<dyn StateStore>,
            vec![Arc::new(PagedCatalogue { total }) as Arc<dyn CatalogueProvider>],
        )
    }

    fn query(title: Option<&str>, start: Option<u32>, end: Option<i64>) -> SongListQuery {
        SongListQuery {
            title: title.map(str::to_owned),
            album_id: None,
            start,
            end,
        }
    }

    fn candidate() -> CatalogueCandidate {
        CatalogueCandidate::new(
            SourceIdentity::new(
                ProviderId::new("youtube".to_owned()).expect("valid provider"),
                SourceScope::Global,
                "vid-1".to_owned(),
            )
            .expect("valid source"),
            TrackMetadata::from_names(
                "song".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
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
                Some(180_000),
            )
            .expect("valid metadata"),
        )
    }

    #[tokio::test]
    async fn auth_gates_search_and_detail() {
        let (catalogue, library) = services(vec![]);
        let auth = auth();
        let result = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            "forged",
            &query(None, None, None),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));

        let result = song_detail(
            &library,
            &auth.service,
            &TrackId::new().to_string(),
            "forged",
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));

        let dead = session::mint(UserId::from_uuid(Uuid::now_v7()), Uuid::now_v7(), &key());
        let result = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            &dead,
            &query(Some("song"), None, None),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::Unauthorized)));
    }

    #[tokio::test]
    async fn search_maps_tracks_short_circuits_empty_and_clamps_ranges() {
        let auth = auth();
        let result = search_songs(
            &catalogue(vec![]),
            &auth.service,
            &context(),
            &auth.token,
            &query(None, None, None),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Ok(songs) if songs.is_empty()));

        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(9)),
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "song");
        assert_eq!(songs[0].artist, "a, b");
        assert_eq!(songs[0].duration, 180.0);
        assert!(!songs[0].id.is_empty());

        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(100)),
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        assert_eq!(songs.len(), 1);
    }

    #[tokio::test]
    async fn an_unbounded_end_is_not_a_search_error() {
        let auth = auth();
        let songs = search_songs(
            &catalogue(vec![Ok(vec![candidate()])]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(-1)),
            SourceTags::Off,
        )
        .await
        .expect("an unbounded end is accepted");
        assert_eq!(songs.len(), 1);
    }

    #[tokio::test]
    async fn bounded_search_window_pages_offsets_and_rejects_beyond() {
        let auth = auth();
        let result = search_songs(
            &catalogue(vec![]),
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(1_001), Some(1_010)),
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::InvalidSearch)));

        let catalogue = paged_catalogue(25);
        async fn songs(
            catalogue: &SearchService,
            auth: &TestAuth,
            context: &RequestContext,
            start: u32,
            end: i64,
        ) -> Vec<Song> {
            search_songs(
                catalogue,
                &auth.service,
                context,
                &auth.token,
                &query(Some("song"), Some(start), Some(end)),
                SourceTags::Off,
            )
            .await
            .expect("page succeeds")
        }
        let context = context();
        let first = songs(&catalogue, &auth, &context, 0, 9).await;
        let second = songs(&catalogue, &auth, &context, 10, 19).await;
        let third = songs(&catalogue, &auth, &context, 20, 29).await;
        let bounded = songs(&catalogue, &auth, &context, 25, 34).await;
        let first_titles = first
            .iter()
            .map(|song| song.title.clone())
            .collect::<Vec<_>>();
        let expected_titles = (0..10)
            .map(|index| format!("song-{index}"))
            .collect::<Vec<_>>();
        assert_eq!(first_titles, expected_titles);
        assert_eq!(second[0].title, "song-10");
        assert_eq!(second.len(), 10);
        assert_eq!(third[0].title, "song-20");
        assert_eq!(third.len(), 5);
        assert!(bounded.is_empty());
    }

    #[tokio::test]
    async fn detail_returns_songs_and_rejects_unknown_ids() {
        let (catalogue, library) = services(vec![Ok(vec![candidate()])]);
        let auth = auth();
        let songs = search_songs(
            &catalogue,
            &auth.service,
            &context(),
            &auth.token,
            &query(Some("song"), Some(0), Some(9)),
            SourceTags::Off,
        )
        .await
        .expect("search succeeds");
        let song = song_detail(
            &library,
            &auth.service,
            &songs[0].id,
            &auth.token,
            SourceTags::Off,
        )
        .await
        .expect("detail succeeds");
        assert_eq!(song.id, songs[0].id);
        assert_eq!(song.title, "song");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.duration, 180.0);

        let result = song_detail(
            &library,
            &auth.service,
            &TrackId::new().to_string(),
            &auth.token,
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::NotFound)));

        let result = song_detail(
            &library,
            &auth.service,
            "not-a-uuid",
            &auth.token,
            SourceTags::Off,
        )
        .await;
        assert!(matches!(result, Err(SongError::NotFound)));
    }

    #[test]
    fn track_maps_to_song() {
        let track = Track::new(
            TrackId::new(),
            TrackMetadata::from_names(
                "title".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
            .expect("valid metadata"),
        );
        let album = crate::tracks::Album::new(
            crate::tracks::AlbumId::new(),
            "album".to_owned(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("valid album");
        let id = track.id().to_string();

        let song = song_from(
            &SearchHit::new(track, Some(album), Vec::new()),
            SourceTags::Off,
        );

        assert_eq!(song.title, "title");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.album.as_deref(), Some("album"));
        assert_eq!(song.cover_art.as_deref(), Some(id.as_str()));
        assert_eq!(song.duration, 180.0);
        assert!(!song.id.is_empty());
    }
}
