use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::features::metadata::domain::{
    AbsenceReason, CandidateItem, FieldId, FieldValue, Freshness, Presence,
};
use crate::features::metadata::{MetadataError, MetadataProfile, MetadataService};
use crate::features::music_library::domain::{
    Artist, ArtistCredit, ArtistId, ArtistSummary, EnrichmentSubject, IdSet, ListenExtent,
    ListenOrder, ListenQuery, ListenWindow, MatchRecord, MatchStatus, PageLimit, ProviderId,
    RecordingIndex, SourceIdentity, SourceScope, Track, TrackHit, TrackId, TrackListFilter,
    TrackListScope, TrackOrder,
};
use crate::features::music_library::storage::contracts::{
    LibraryRead, ListenStore, SourceLookup, StateStore, TrackRepositoryError,
};

const MUSICBRAINZ_PROVIDER: &str = "musicbrainz";
const LISTENBRAINZ_PROVIDER: &str = "listenbrainz";
const LISTEN_SCAN_LIMIT: u32 = 500;
const TRACK_SCAN_LIMIT: u32 = 500;
const METADATA_LANGUAGE: &str = "en";
const PERSONAL_WINDOW_SCOPE: &str = "personal listen window";
const LIBRARY_SCOPE: &str = "shared server facts, no time dimension";
const TRACK_CREDIT_PEERS_SCOPE: &str = "shared track artist credits, no time dimension";
const LIBRARY_TITLE_ORDER_SCOPE: &str = "owned tracks in title order";
const SIMILAR_TRACKS_SCOPE: &str =
    "related artists in basis rank order, local top tracks per artist";
const ALL_TIME_SCOPE: &str = "all-time play counter";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryBasis {
    Personal,
    Library,
    External,
}

impl DiscoveryBasis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Library => "library",
            Self::External => "external",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "personal" => Some(Self::Personal),
            "library" => Some(Self::Library),
            "external" => Some(Self::External),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoverySeed {
    Artist(ArtistId),
    Track(TrackId),
}

impl DiscoverySeed {
    pub fn artist_id(&self) -> Option<&ArtistId> {
        match self {
            Self::Artist(artist_id) => Some(artist_id),
            Self::Track(_) => None,
        }
    }

    pub fn track_id(&self) -> Option<&TrackId> {
        match self {
            Self::Track(track_id) => Some(track_id),
            Self::Artist(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedRequest {
    pub seed: DiscoverySeed,
    pub basis: DiscoveryBasis,
    pub window: Option<ListenWindow>,
    pub limit: PageLimit,
    pub user: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopRequest {
    pub seed: DiscoverySeed,
    pub basis: DiscoveryBasis,
    pub window: Option<ListenWindow>,
    pub limit: PageLimit,
    pub user: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelatedItem {
    pub canonical: Option<ArtistId>,
    pub name: String,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub rank_in_basis: u32,
    pub ranking_scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarTracksRequest {
    pub seed: DiscoverySeed,
    pub basis: DiscoveryBasis,
    pub window: Option<ListenWindow>,
    pub limit: PageLimit,
    pub user: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopItem {
    pub canonical: Option<TrackId>,
    pub title: String,
    pub artist: String,
    pub provider: Option<String>,
    pub external_id: Option<String>,
    pub retrieved_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub rank_in_basis: u32,
    pub ranking_scope: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    NoProviderFacts,
    ProviderStale,
    ProviderFailed,
    ProviderNotLicensed,
    ProviderCapabilityAbsent,
    ProviderIdentityUnestablished,
    ProviderCredentialMissing,
    ProviderConfigurationDisabled,
    ProviderScopeUnnamed,
    NoLocalBasis,
}

impl FallbackReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoProviderFacts => "no_provider_facts",
            Self::ProviderStale => "provider_stale",
            Self::ProviderFailed => "provider_failed",
            Self::ProviderNotLicensed => "provider_not_licensed",
            Self::ProviderCapabilityAbsent => "provider_capability_absent",
            Self::ProviderIdentityUnestablished => "provider_identity_unestablished",
            Self::ProviderCredentialMissing => "provider_credential_missing",
            Self::ProviderConfigurationDisabled => "provider_configuration_disabled",
            Self::ProviderScopeUnnamed => "provider_scope_unnamed",
            Self::NoLocalBasis => "no_local_basis",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryResult<T> {
    pub basis: DiscoveryBasis,
    pub window: Option<ListenExtent>,
    pub coverage_since: Option<DateTime<Utc>>,
    pub ranking_scope: Option<String>,
    pub fallback: bool,
    pub fallback_reason: Option<FallbackReason>,
    pub items: Vec<T>,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("basis `{basis}` is not offered")]
    UnsupportedBasis { basis: &'static str },
    #[error("the discovery seed does not exist")]
    UnknownSeed,
    #[error("discovery storage is unavailable")]
    Unavailable,
    #[error("discovery repository failed")]
    Repository(#[source] TrackRepositoryError),
}

pub struct DiscoveryService {
    library: Arc<dyn LibraryRead>,
    sources: Arc<dyn SourceLookup>,
    _state: Arc<dyn StateStore>,
    listens: Arc<dyn ListenStore>,
    metadata: Option<Arc<MetadataService>>,
    now: DateTime<Utc>,
}

impl DiscoveryService {
    pub fn new(
        library: Arc<dyn LibraryRead>,
        sources: Arc<dyn SourceLookup>,
        state: Arc<dyn StateStore>,
        listens: Arc<dyn ListenStore>,
    ) -> Self {
        Self {
            library,
            sources,
            _state: state,
            listens,
            metadata: None,
            now: Utc::now(),
        }
    }

    pub fn with_metadata(mut self, metadata: Arc<MetadataService>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn at(mut self, now: DateTime<Utc>) -> Self {
        self.now = now;
        self
    }

    pub async fn related(
        &self,
        request: RelatedRequest,
    ) -> Result<DiscoveryResult<RelatedItem>, DiscoveryError> {
        let seed = self.seed_artist(&request.seed).await?;
        let excluded = seed.id().clone();

        match request.basis {
            DiscoveryBasis::Library => self.library_related(&seed, &excluded, &request).await,
            DiscoveryBasis::Personal => self.personal_related(&seed, &excluded, &request).await,
            DiscoveryBasis::External => self.external_related(&seed, &excluded, &request).await,
        }
    }

    pub async fn top(
        &self,
        request: TopRequest,
    ) -> Result<DiscoveryResult<TopItem>, DiscoveryError> {
        let seed = self.seed_artist(&request.seed).await?;

        match request.basis {
            DiscoveryBasis::Library => {
                let items = self.library_top(&seed, &request.seed, &request).await?;
                let mut result = DiscoveryResult {
                    basis: DiscoveryBasis::Library,
                    window: None,
                    coverage_since: None,
                    ranking_scope: Some(LIBRARY_TITLE_ORDER_SCOPE.to_owned()),
                    fallback: false,
                    fallback_reason: None,
                    items,
                };
                if result.items.is_empty() {
                    result.fallback = true;
                    result.fallback_reason = Some(FallbackReason::NoLocalBasis);
                }

                Ok(result)
            }
            DiscoveryBasis::Personal => self.personal_top(&seed, &request.seed, &request).await,
            DiscoveryBasis::External => self.external_top(&seed, &request.seed, &request).await,
        }
    }

    pub async fn similar_tracks(
        &self,
        request: SimilarTracksRequest,
    ) -> Result<DiscoveryResult<TopItem>, DiscoveryError> {
        let seed = self.seed_artist(&request.seed).await?;
        let excluded = seed.id().clone();
        let related_request = RelatedRequest {
            seed: request.seed.clone(),
            basis: request.basis,
            window: request.window,
            limit: request.limit,
            user: request.user,
        };
        let related = match request.basis {
            DiscoveryBasis::Library => {
                self.library_related(&seed, &excluded, &related_request)
                    .await?
            }
            DiscoveryBasis::Personal => {
                self.personal_related(&seed, &excluded, &related_request)
                    .await?
            }
            DiscoveryBasis::External => {
                return Err(Self::unsupported_basis(request.basis.as_str()));
            }
        };
        let related_basis = related.basis;
        let related_window = related.window;
        let related_coverage = related.coverage_since;
        let related_reason = related.fallback_reason;

        let top = TopRequest {
            seed: request.seed.clone(),
            basis: request.basis,
            window: request.window,
            limit: request.limit,
            user: request.user,
        };
        let mut items = self.similar_track_items(related.items, &top).await?;
        finalise(&mut items, request.limit);

        let mut result = DiscoveryResult {
            basis: related_basis,
            window: related_window,
            coverage_since: related_coverage,
            ranking_scope: Some(SIMILAR_TRACKS_SCOPE.to_owned()),
            fallback: false,
            fallback_reason: None,
            items,
        };
        if result.items.is_empty() {
            result.fallback = true;
            result.fallback_reason = Some(related_reason.unwrap_or(FallbackReason::NoLocalBasis));
        }

        Ok(result)
    }

    async fn similar_track_items(
        &self,
        related: Vec<RelatedItem>,
        top: &TopRequest,
    ) -> Result<Vec<TopItem>, DiscoveryError> {
        let limit = usize::try_from(top.limit.get()).unwrap_or(usize::MAX);
        let mut items = Vec::new();
        let mut emitted = HashSet::new();
        for candidate in related {
            if items.len() >= limit {
                break;
            }
            let Some(artist_id) = candidate.canonical else {
                continue;
            };
            let Some(artist) = self
                .library
                .find_artist(&artist_id, None)
                .await
                .map_err(map_error)?
            else {
                continue;
            };
            for mut item in self.library_top(&artist, &top.seed, top).await? {
                if items.len() >= limit {
                    break;
                }
                let Some(track_id) = item.canonical.clone() else {
                    continue;
                };
                if !emitted.insert(track_id) {
                    continue;
                }
                item.ranking_scope = Some(SIMILAR_TRACKS_SCOPE.to_owned());
                items.push(item);
            }
        }

        Ok(items)
    }

    async fn seed_artist(&self, seed: &DiscoverySeed) -> Result<Artist, DiscoveryError> {
        let artist_id = match seed {
            DiscoverySeed::Artist(artist_id) => artist_id.clone(),
            DiscoverySeed::Track(track_id) => {
                let Some(track) = self.library.find_track(track_id).await.map_err(map_error)?
                else {
                    return Err(DiscoveryError::UnknownSeed);
                };
                let credit = track
                    .metadata()
                    .credits()
                    .first()
                    .and_then(|credit| credit.musicbrainz_artist_id())
                    .and_then(musicbrainz_source);
                let Some(source) = credit else {
                    return Err(DiscoveryError::UnknownSeed);
                };
                let Some(artist) = self
                    .sources
                    .find_artist_by_source(&source)
                    .await
                    .map_err(map_error)?
                else {
                    return Err(DiscoveryError::UnknownSeed);
                };

                artist.id().clone()
            }
        };

        self.library
            .find_artist(&artist_id, None)
            .await
            .map_err(map_error)?
            .ok_or(DiscoveryError::UnknownSeed)
    }

    async fn seed_tracks(&self, artist: &Artist) -> Result<Vec<TrackId>, DiscoveryError> {
        let page = self
            .library
            .page_tracks(
                0,
                TRACK_SCAN_LIMIT,
                TrackOrder::Id,
                &TrackListFilter::new()
                    .with_scope(TrackListScope::Owned)
                    .with_artists_id(Some(id_set(vec![artist.id().clone()]))),
                None,
            )
            .await
            .map_err(map_error)?;

        Ok(page
            .items()
            .iter()
            .map(|hit| hit.track().id().clone())
            .collect())
    }

    async fn credit_peers(&self, tracks: &[TrackId]) -> Result<Vec<ArtistId>, DiscoveryError> {
        let mut artists = HashSet::new();
        for view in self
            .library
            .track_views(tracks, None)
            .await
            .map_err(map_error)?
        {
            for credit in view.track().metadata().credits() {
                if let Some(artist_id) = self.credit_artist(credit).await? {
                    artists.insert(artist_id);
                }
            }
        }

        Ok(artists.into_iter().collect())
    }

    async fn credit_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Option<ArtistId>, DiscoveryError> {
        let Some(source) = credit.musicbrainz_artist_id().and_then(musicbrainz_source) else {
            return Ok(None);
        };

        self.sources
            .find_artist_by_source(&source)
            .await
            .map(|artist| artist.map(|artist| artist.id().clone()))
            .map_err(map_error)
    }

    async fn library_candidates(
        &self,
        artist: &Artist,
    ) -> Result<Vec<(ArtistId, i64)>, DiscoveryError> {
        let counts = self.credit_relation(artist).await?;

        Ok(ranked(counts))
    }

    async fn credit_relation(
        &self,
        artist: &Artist,
    ) -> Result<HashMap<ArtistId, i64>, DiscoveryError> {
        let tracks = self.seed_tracks(artist).await?;
        let credits = self
            .credited_tracks(std::slice::from_ref(artist.id()))
            .await?;
        let mut all = tracks;
        for track_id in credits {
            if !all.contains(&track_id) {
                all.push(track_id);
            }
        }
        let mut counts: HashMap<ArtistId, i64> = HashMap::new();
        for peer in self.credit_peers(&all).await? {
            *counts.entry(peer).or_default() += 1;
        }

        Ok(counts)
    }

    async fn credited_tracks(
        &self,
        artist_ids: &[ArtistId],
    ) -> Result<Vec<TrackId>, DiscoveryError> {
        self.library
            .tracks_credited_to(artist_ids)
            .await
            .map_err(map_error)
    }

    async fn library_related(
        &self,
        artist: &Artist,
        excluded: &ArtistId,
        request: &RelatedRequest,
    ) -> Result<DiscoveryResult<RelatedItem>, DiscoveryError> {
        let mut items = Vec::new();
        for (artist_id, _) in self.library_candidates(artist).await? {
            if &artist_id == excluded {
                continue;
            }
            let Some(candidate) = self.artist_summary(&artist_id).await? else {
                continue;
            };
            items.push(RelatedItem {
                canonical: Some(artist_id),
                name: candidate.artist().name().to_owned(),
                provider: None,
                external_id: None,
                retrieved_at: None,
                score: None,
                rank_in_basis: 0,
                ranking_scope: Some(TRACK_CREDIT_PEERS_SCOPE.to_owned()),
            });
        }
        finalise(&mut items, request.limit);

        let mut result = DiscoveryResult {
            basis: DiscoveryBasis::Library,
            window: None,
            coverage_since: None,
            ranking_scope: Some(TRACK_CREDIT_PEERS_SCOPE.to_owned()),
            fallback: false,
            fallback_reason: None,
            items,
        };
        if result.items.is_empty() {
            result.fallback = true;
            result.fallback_reason = Some(FallbackReason::NoLocalBasis);
        }

        Ok(result)
    }

    async fn library_top(
        &self,
        artist: &Artist,
        excluded: &DiscoverySeed,
        request: &TopRequest,
    ) -> Result<Vec<TopItem>, DiscoveryError> {
        let page = self
            .library
            .page_tracks(
                0,
                TRACK_SCAN_LIMIT,
                TrackOrder::Title,
                &TrackListFilter::new()
                    .with_scope(TrackListScope::Owned)
                    .with_artists_id(Some(id_set(vec![artist.id().clone()]))),
                None,
            )
            .await
            .map_err(map_error)?;
        let views = self
            .library
            .track_views(
                &page
                    .items()
                    .iter()
                    .map(|hit| hit.track().id().clone())
                    .collect::<Vec<_>>(),
                None,
            )
            .await
            .map_err(map_error)?;
        let mut items = Vec::new();
        for view in views {
            if excluded.track_id() == Some(view.track().id()) {
                continue;
            }
            items.push(top_item(
                view.track().id().clone(),
                view.track().title().to_owned(),
                artist_names(view.track()),
                LIBRARY_TITLE_ORDER_SCOPE,
            ));
        }
        finalise(&mut items, request.limit);

        Ok(items)
    }

    async fn personal_related(
        &self,
        artist: &Artist,
        excluded: &ArtistId,
        request: &RelatedRequest,
    ) -> Result<DiscoveryResult<RelatedItem>, DiscoveryError> {
        let Some(user) = request.user else {
            return self.personal_unavailable(request);
        };
        let played = self.played_tracks(user, request.window).await?;
        if played.is_empty() {
            return self.personal_unavailable(request);
        }
        let seed_credits = self
            .credited_tracks(std::slice::from_ref(artist.id()))
            .await?;
        let seed_plays = played
            .iter()
            .filter(|track_id| seed_credits.contains(track_id))
            .cloned()
            .collect::<Vec<_>>();
        if seed_plays.is_empty() {
            return self.personal_unavailable(request);
        }

        let mut counts: HashMap<ArtistId, i64> = HashMap::new();
        for peer in self.credit_peers(&seed_plays).await? {
            *counts.entry(peer).or_default() += 1;
        }

        let mut items = Vec::new();
        for (artist_id, _) in ranked(counts) {
            if &artist_id == excluded {
                continue;
            }
            let Some(candidate) = self.artist_summary(&artist_id).await? else {
                continue;
            };
            items.push(RelatedItem {
                canonical: Some(artist_id),
                name: candidate.artist().name().to_owned(),
                provider: None,
                external_id: None,
                retrieved_at: None,
                score: None,
                rank_in_basis: 0,
                ranking_scope: Some(PERSONAL_WINDOW_SCOPE.to_owned()),
            });
        }
        finalise(&mut items, request.limit);

        let mut result = DiscoveryResult {
            basis: DiscoveryBasis::Personal,
            window: listening_window(request.window, self.now),
            coverage_since: self.coverage(user, request.window).await?,
            ranking_scope: Some(PERSONAL_WINDOW_SCOPE.to_owned()),
            fallback: false,
            fallback_reason: None,
            items,
        };
        if result.items.is_empty() {
            result.fallback = true;
            result.fallback_reason = Some(FallbackReason::NoLocalBasis);
        }

        Ok(result)
    }

    fn personal_unavailable<T>(
        &self,
        request: &RelatedRequest,
    ) -> Result<DiscoveryResult<T>, DiscoveryError> {
        Ok(DiscoveryResult {
            basis: DiscoveryBasis::Personal,
            window: listening_window(request.window, self.now),
            coverage_since: None,
            ranking_scope: Some(PERSONAL_WINDOW_SCOPE.to_owned()),
            fallback: true,
            fallback_reason: Some(FallbackReason::NoLocalBasis),
            items: Vec::new(),
        })
    }

    async fn personal_top(
        &self,
        artist: &Artist,
        excluded: &DiscoverySeed,
        request: &TopRequest,
    ) -> Result<DiscoveryResult<TopItem>, DiscoveryError> {
        let Some(user) = request.user else {
            return Ok(DiscoveryResult {
                basis: DiscoveryBasis::Personal,
                window: listening_window(request.window, self.now),
                coverage_since: None,
                ranking_scope: Some(PERSONAL_WINDOW_SCOPE.to_owned()),
                fallback: true,
                fallback_reason: Some(FallbackReason::NoLocalBasis),
                items: Vec::new(),
            });
        };
        let window = request.window.unwrap_or_else(ListenWindow::unbounded);
        let (counts, scope, listen_extent) = if window.is_unbounded() {
            (
                self.all_time_counts(user, artist).await?,
                ALL_TIME_SCOPE,
                None,
            )
        } else {
            let counts = self.window_counts(user, artist, window).await?;
            (
                counts,
                PERSONAL_WINDOW_SCOPE,
                listening_window(request.window, self.now),
            )
        };

        let mut items = Vec::new();
        for (track_id, count) in counts {
            if excluded.track_id() == Some(&track_id) {
                continue;
            }
            let Some(track) = self
                .library
                .find_track(&track_id)
                .await
                .map_err(map_error)?
            else {
                continue;
            };
            let mut item = top_item(
                track_id,
                track.title().to_owned(),
                artist_names(&track),
                scope,
            );
            item.score = Some(count as f64);
            items.push(item);
        }
        finalise(&mut items, request.limit);

        let mut result = DiscoveryResult {
            basis: DiscoveryBasis::Personal,
            window: listen_extent,
            coverage_since: self.coverage(user, request.window).await?,
            ranking_scope: Some(scope.to_owned()),
            fallback: false,
            fallback_reason: None,
            items,
        };
        if result.items.is_empty() {
            result.fallback = true;
            result.fallback_reason = Some(FallbackReason::NoLocalBasis);
        }

        Ok(result)
    }

    async fn played_tracks(
        &self,
        user: Uuid,
        window: Option<ListenWindow>,
    ) -> Result<Vec<TrackId>, DiscoveryError> {
        let window = window.unwrap_or_else(ListenWindow::unbounded);
        let query = ListenQuery::new(window, ListenOrder::NewestFirst, scan_limit(), 0);
        let page = self
            .listens
            .listen_history(user, &query)
            .await
            .map_err(map_error)?;

        Ok(page
            .entries()
            .iter()
            .map(|record| record.track_id().clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect())
    }

    async fn window_counts(
        &self,
        user: Uuid,
        artist: &Artist,
        window: ListenWindow,
    ) -> Result<Vec<(TrackId, u64)>, DiscoveryError> {
        let owned = self.owned_tracks(artist).await?;
        let query = ListenQuery::new(window, ListenOrder::NewestFirst, scan_limit(), 0);
        let page = self
            .listens
            .listen_history(user, &query)
            .await
            .map_err(map_error)?;
        let mut counts: HashMap<TrackId, u64> = HashMap::new();
        for record in page.entries() {
            if owned.contains(record.track_id()) {
                *counts.entry(record.track_id().clone()).or_default() += 1;
            }
        }

        Ok(ranked_counts(counts))
    }

    async fn all_time_counts(
        &self,
        user: Uuid,
        artist: &Artist,
    ) -> Result<Vec<(TrackId, u64)>, DiscoveryError> {
        let views = self
            .library
            .track_views(&self.owned_tracks(artist).await?, Some(user))
            .await
            .map_err(map_error)?;
        let mut counts = HashMap::new();
        for view in views {
            let count = u64::from(view.state().play_count());
            if count > 0 {
                counts.insert(view.track().id().clone(), count);
            }
        }

        Ok(ranked_counts(counts))
    }

    async fn owned_tracks(&self, artist: &Artist) -> Result<Vec<TrackId>, DiscoveryError> {
        let page = self
            .library
            .page_tracks(
                0,
                TRACK_SCAN_LIMIT,
                TrackOrder::Id,
                &TrackListFilter::new()
                    .with_scope(TrackListScope::Owned)
                    .with_artists_id(Some(id_set(vec![artist.id().clone()]))),
                None,
            )
            .await
            .map_err(map_error)?;

        Ok(page
            .items()
            .iter()
            .map(|hit| hit.track().id().clone())
            .collect())
    }

    async fn coverage(
        &self,
        user: Uuid,
        window: Option<ListenWindow>,
    ) -> Result<Option<DateTime<Utc>>, DiscoveryError> {
        let marker = self
            .listens
            .listen_count(user, ListenWindow::unbounded(), None)
            .await
            .map_err(map_error)?
            .coverage()
            .since();

        Ok(window
            .unwrap_or_else(ListenWindow::unbounded)
            .covered_start(marker))
    }

    async fn external_related(
        &self,
        artist: &Artist,
        excluded: &ArtistId,
        request: &RelatedRequest,
    ) -> Result<DiscoveryResult<RelatedItem>, DiscoveryError> {
        let read = self.provider_candidates(artist, FieldId::Similar).await?;
        let facts = match read {
            ProviderRead::Facts(facts) => facts,
            ProviderRead::Fallback(reason) => {
                let items = self
                    .library_related_items(artist, excluded, request.limit)
                    .await?;

                return Ok(DiscoveryResult {
                    basis: DiscoveryBasis::Library,
                    window: None,
                    coverage_since: None,
                    ranking_scope: Some(LIBRARY_SCOPE.to_owned()),
                    fallback: true,
                    fallback_reason: Some(reason),
                    items,
                });
            }
        };
        let ProviderFacts {
            candidates,
            provider,
            scope,
            retrieved_at,
        } = facts;

        let mut items = Vec::new();
        for (rank, candidate) in candidates.into_iter().enumerate() {
            let canonical = match candidate
                .external_id
                .as_deref()
                .and_then(|external_id| identity(&provider, external_id))
            {
                Some(source) => self
                    .sources
                    .find_artist_by_source(&source)
                    .await
                    .map(|artist| artist.map(|artist| artist.id().clone()))
                    .map_err(map_error)?,
                None => None,
            };
            let canonical = match canonical {
                Some(artist_id) if &artist_id != excluded => {
                    match self.artist_summary(&artist_id).await? {
                        Some(_) => Some(artist_id),
                        None => continue,
                    }
                }
                Some(_) => continue,
                None => None,
            };
            items.push(RelatedItem {
                canonical,
                name: candidate.name.clone(),
                provider: Some(provider.clone()),
                external_id: candidate.external_id,
                retrieved_at: Some(retrieved_at),
                score: candidate.score,
                rank_in_basis: u32::try_from(rank).unwrap_or(u32::MAX),
                ranking_scope: Some(scope.clone()),
            });
        }
        items.truncate(usize::try_from(request.limit.get()).unwrap_or(usize::MAX));

        Ok(DiscoveryResult {
            basis: DiscoveryBasis::External,
            window: None,
            coverage_since: None,
            ranking_scope: Some(scope),
            fallback: false,
            fallback_reason: None,
            items,
        })
    }

    async fn external_top(
        &self,
        artist: &Artist,
        excluded: &DiscoverySeed,
        request: &TopRequest,
    ) -> Result<DiscoveryResult<TopItem>, DiscoveryError> {
        let read = self.provider_candidates(artist, FieldId::TopTracks).await?;
        let facts = match read {
            ProviderRead::Facts(facts) => facts,
            ProviderRead::Fallback(reason) => {
                let mut local = self.library_top(artist, excluded, request).await?;
                finalise(&mut local, request.limit);

                return Ok(DiscoveryResult {
                    basis: DiscoveryBasis::Library,
                    window: None,
                    coverage_since: None,
                    ranking_scope: Some(LIBRARY_SCOPE.to_owned()),
                    fallback: true,
                    fallback_reason: Some(reason),
                    items: local,
                });
            }
        };
        let ProviderFacts {
            candidates,
            provider,
            scope,
            retrieved_at,
        } = facts;

        let records = candidates
            .iter()
            .filter_map(remote_record)
            .collect::<Vec<_>>();
        let keys = records
            .iter()
            .map(MatchRecord::retrieval_key)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let local = self
            .library
            .dedup_candidates(&keys, TRACK_SCAN_LIMIT)
            .await
            .map_err(map_error)?;
        let index = RecordingIndex::new(local.iter().map(local_record).collect());
        let now = self.now.timestamp();
        let mut items = Vec::new();
        for (rank, candidate) in candidates.into_iter().enumerate() {
            let external_id = candidate.external_id.clone();
            let canonical = external_id
                .as_deref()
                .and_then(|id| records.iter().find(|record| record.id() == id))
                .map(|record| index.match_record(record, now))
                .filter(|decision| decision.status() == MatchStatus::Verified)
                .and_then(|decision| {
                    decision
                        .target()
                        .and_then(|target| target.parse::<TrackId>().ok())
                });
            if canonical.is_some() && canonical.as_ref() == excluded.track_id() {
                continue;
            }
            items.push(TopItem {
                canonical,
                title: candidate.name,
                artist: candidate.artists.join(", "),
                provider: Some(provider.clone()),
                external_id,
                retrieved_at: Some(retrieved_at),
                score: candidate.score,
                rank_in_basis: u32::try_from(rank).unwrap_or(u32::MAX),
                ranking_scope: Some(scope.clone()),
            });
        }
        items.truncate(usize::try_from(request.limit.get()).unwrap_or(usize::MAX));

        Ok(DiscoveryResult {
            basis: DiscoveryBasis::External,
            window: None,
            coverage_since: None,
            ranking_scope: Some(scope),
            fallback: false,
            fallback_reason: None,
            items,
        })
    }

    async fn provider_candidates(
        &self,
        artist: &Artist,
        field: FieldId,
    ) -> Result<ProviderRead, DiscoveryError> {
        let Some(metadata) = self.metadata.as_ref() else {
            return Ok(ProviderRead::Fallback(FallbackReason::NoProviderFacts));
        };
        let subject = self.subject(artist).await?;
        let record = match metadata
            .read_artist(
                &subject,
                &MetadataProfile::artist_display(),
                METADATA_LANGUAGE,
                self.now,
            )
            .await
        {
            Ok(record) => record,
            Err(MetadataError::Disabled | MetadataError::DisabledSource) => {
                return Ok(ProviderRead::Fallback(FallbackReason::ProviderNotLicensed));
            }
            Err(MetadataError::ProviderUnavailable) => {
                return Ok(ProviderRead::Fallback(FallbackReason::ProviderFailed));
            }
            Err(MetadataError::Unavailable | MetadataError::InvalidData) => {
                return Err(DiscoveryError::Unavailable);
            }
        };
        let Some(entry) = record.fields.iter().find(|entry| entry.field == field) else {
            return Ok(ProviderRead::Fallback(FallbackReason::NoProviderFacts));
        };

        if entry.presence == Presence::Absent {
            return Ok(ProviderRead::Fallback(absence_reason(entry.absence)));
        }
        match entry.freshness {
            Freshness::Failed(_) => {
                return Ok(ProviderRead::Fallback(FallbackReason::ProviderFailed));
            }
            Freshness::Stale => return Ok(ProviderRead::Fallback(FallbackReason::ProviderStale)),
            Freshness::Fresh | Freshness::Unrequested => {}
        }

        let candidates = match entry.value.as_ref() {
            Some(FieldValue::Candidates(candidates)) => candidates.clone(),
            _ => Vec::new(),
        };
        if candidates.is_empty() {
            return Ok(ProviderRead::Fallback(FallbackReason::NoProviderFacts));
        }
        let provider = entry
            .source
            .as_ref()
            .map(|source| source.provider_id.clone())
            .unwrap_or_default();
        if provider.is_empty() {
            return Ok(ProviderRead::Fallback(FallbackReason::NoProviderFacts));
        }
        if provider == LISTENBRAINZ_PROVIDER
            && !entry
                .licence
                .as_ref()
                .is_some_and(|terms| terms.redistribution_allowed)
        {
            return Ok(ProviderRead::Fallback(FallbackReason::ProviderNotLicensed));
        }
        let scope = candidates
            .iter()
            .filter_map(|candidate| candidate.ranking_scope.as_deref())
            .find(|scope| !scope.is_empty())
            .map(str::to_owned);
        let Some(scope) = scope else {
            return Ok(ProviderRead::Fallback(FallbackReason::ProviderScopeUnnamed));
        };

        Ok(ProviderRead::Facts(ProviderFacts {
            candidates,
            provider,
            scope,
            retrieved_at: entry.retrieved_at.unwrap_or(self.now),
        }))
    }

    async fn subject(&self, artist: &Artist) -> Result<EnrichmentSubject, DiscoveryError> {
        let identities = self
            .sources
            .find_artist_sources(artist.id())
            .await
            .map_err(map_error)?;

        Ok(EnrichmentSubject::new(
            artist.id().clone(),
            artist.name().to_owned(),
            artist.musicbrainz_artist_id().map(str::to_owned),
            identities,
        ))
    }

    async fn artist_summary(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Option<ArtistSummary>, DiscoveryError> {
        self.library
            .find_artist(artist_id, None)
            .await
            .map_err(map_error)
            .map(|artist| artist.map(|artist| ArtistSummary::new(artist, 0)))
    }

    async fn library_related_items(
        &self,
        artist: &Artist,
        excluded: &ArtistId,
        limit: PageLimit,
    ) -> Result<Vec<RelatedItem>, DiscoveryError> {
        let mut items = Vec::new();
        for (artist_id, _) in self.library_candidates(artist).await? {
            if &artist_id == excluded {
                continue;
            }
            let Some(candidate) = self.artist_summary(&artist_id).await? else {
                continue;
            };
            items.push(RelatedItem {
                canonical: Some(artist_id),
                name: candidate.artist().name().to_owned(),
                provider: None,
                external_id: None,
                retrieved_at: None,
                score: None,
                rank_in_basis: 0,
                ranking_scope: Some(TRACK_CREDIT_PEERS_SCOPE.to_owned()),
            });
            if items.len() >= usize::try_from(limit.get()).unwrap_or(usize::MAX) {
                break;
            }
        }
        finalise(&mut items, limit);

        Ok(items)
    }
}

impl DiscoveryService {
    pub fn unsupported_basis(basis: &'static str) -> DiscoveryError {
        DiscoveryError::UnsupportedBasis { basis }
    }
}

enum ProviderRead {
    Facts(ProviderFacts),
    Fallback(FallbackReason),
}

struct ProviderFacts {
    candidates: Vec<CandidateItem>,
    provider: String,
    scope: String,
    retrieved_at: DateTime<Utc>,
}

fn top_item(track_id: TrackId, title: String, artist: String, scope: &str) -> TopItem {
    TopItem {
        canonical: Some(track_id),
        title,
        artist,
        provider: None,
        external_id: None,
        retrieved_at: None,
        score: None,
        rank_in_basis: 0,
        ranking_scope: Some(scope.to_owned()),
    }
}

fn artist_names(track: &Track) -> String {
    track
        .artists()
        .iter()
        .map(|artist| artist.name())
        .collect::<Vec<_>>()
        .join(", ")
}

fn finalise<T>(items: &mut Vec<T>, limit: PageLimit)
where
    T: Ranked,
{
    items.truncate(usize::try_from(limit.get()).unwrap_or(usize::MAX));
    for (rank, item) in items.iter_mut().enumerate() {
        item.set_rank(u32::try_from(rank).unwrap_or(u32::MAX));
    }
}

trait Ranked {
    fn set_rank(&mut self, rank: u32);
}

impl Ranked for RelatedItem {
    fn set_rank(&mut self, rank: u32) {
        self.rank_in_basis = rank;
    }
}

impl Ranked for TopItem {
    fn set_rank(&mut self, rank: u32) {
        self.rank_in_basis = rank;
    }
}

fn remote_record(candidate: &CandidateItem) -> Option<MatchRecord> {
    let external_id = candidate.external_id.as_ref()?;

    Some(MatchRecord::new(
        external_id.clone(),
        candidate.name.clone(),
        candidate.artists.clone(),
        candidate.duration_ms,
        candidate.release_name.clone().unwrap_or_default(),
        candidate
            .recording_id
            .clone()
            .or_else(|| candidate.external_id.clone()),
        candidate.isrc.clone(),
        None,
        true,
    ))
}

fn local_record(hit: &TrackHit) -> MatchRecord {
    MatchRecord::new(
        hit.track().id().to_string(),
        hit.track().title(),
        hit.track()
            .artists()
            .iter()
            .map(|artist| artist.name().to_owned())
            .collect(),
        hit.track().duration_ms(),
        hit.album()
            .map(|album| album.title().to_owned())
            .unwrap_or_default(),
        hit.track().musicbrainz_recording_id().map(str::to_owned),
        hit.track().isrc().map(str::to_owned),
        None,
        true,
    )
}

pub fn absence_reason(absence: Option<AbsenceReason>) -> FallbackReason {
    match absence {
        Some(AbsenceReason::NotLicensed) => FallbackReason::ProviderNotLicensed,
        Some(AbsenceReason::CapabilityAbsent) => FallbackReason::ProviderCapabilityAbsent,
        Some(AbsenceReason::IdentityUnestablished) => FallbackReason::ProviderIdentityUnestablished,
        Some(AbsenceReason::CredentialMissing) => FallbackReason::ProviderCredentialMissing,
        Some(AbsenceReason::StaleAbsent) => FallbackReason::ProviderStale,
        _ => FallbackReason::NoProviderFacts,
    }
}

fn ranked(counts: HashMap<ArtistId, i64>) -> Vec<(ArtistId, i64)> {
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_id, left), (right_id, right)| {
        right
            .cmp(left)
            .then_with(|| left_id.to_string().cmp(&right_id.to_string()))
    });

    ranked
}

fn ranked_counts(counts: HashMap<TrackId, u64>) -> Vec<(TrackId, u64)> {
    let mut ranked = counts.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|(left_id, left), (right_id, right)| {
        right
            .cmp(left)
            .then_with(|| left_id.to_string().cmp(&right_id.to_string()))
    });

    ranked
}

fn listening_window(window: Option<ListenWindow>, now: DateTime<Utc>) -> Option<ListenExtent> {
    let window = window?;
    if window.is_unbounded() {
        return None;
    }
    let since = window.since().unwrap_or(now);
    let until = window.until().unwrap_or(now);
    let span = until.signed_duration_since(since).num_milliseconds().max(0);

    Some(ListenExtent::new(
        0,
        u64::try_from(span).unwrap_or_default(),
        true,
    ))
}

fn scan_limit() -> PageLimit {
    PageLimit::new(LISTEN_SCAN_LIMIT).expect("the listen scan limit is valid")
}

fn id_set<T: Clone + Eq + std::hash::Hash>(values: Vec<T>) -> IdSet<T> {
    IdSet::new(values).expect("a non-empty selector set")
}

fn musicbrainz_source(identifier: &str) -> Option<SourceIdentity> {
    identity(MUSICBRAINZ_PROVIDER, identifier)
}

fn identity(provider: &str, external_id: &str) -> Option<SourceIdentity> {
    let provider = ProviderId::new(provider.to_owned()).ok()?;

    SourceIdentity::new(provider, SourceScope::Global, external_id.to_owned()).ok()
}

fn map_error(error: TrackRepositoryError) -> DiscoveryError {
    match error {
        TrackRepositoryError::Unavailable => DiscoveryError::Unavailable,
        error => DiscoveryError::Repository(error),
    }
}
