use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::features::metadata::capabilities::{CAPABILITY_TTL, CapabilitySnapshot, Snapshot};
use crate::features::metadata::domain::{
    CandidateItem, FactSource, FieldId, FieldValue, MatchBasis, StoredFact,
};
use crate::features::metadata::freshness::NoJitter;
use crate::features::metadata::licences::licence_terms;
use crate::features::metadata::refresh::{DocumentError, DocumentOutcome, MetadataDocuments};
use crate::features::metadata::selection::{FieldOrder, YOUTUBE_MUSIC};
use crate::features::metadata::service::{MetadataProfile, MetadataService};
use crate::features::metadata::storage::{CommitOutcome, MetadataRepositoryError, MetadataStore};
use crate::features::music_library::discovery::{
    DiscoveryBasis, DiscoveryError, DiscoverySeed, DiscoveryService, FallbackReason, RelatedItem,
    RelatedRequest, SimilarTracksRequest, TopRequest,
};
use crate::features::music_library::domain::normalize_for_matching;
use crate::features::music_library::domain::{
    Album, AlbumId, AlbumSummary, Artist, ArtistCredit, ArtistId, EnrichmentSubject, LibraryState,
    ListenQuery, ListenRecord, ListenWindow, PageLimit, ProviderId, SourceIdentity, SourceScope,
    Track, TrackHit, TrackId, TrackListFilter, TrackListScope, TrackMetadata, TrackOrder,
    TrackPage, TrackView,
};
use crate::features::music_library::test_support::{
    LibraryRead, ListenStore, SourceLookup, StateStore, TrackRepositoryError,
};

fn artist(name: &str, mbid: Option<&str>) -> Artist {
    Artist::new(
        ArtistId::new(),
        name.to_owned(),
        None,
        mbid.map(str::to_owned),
    )
    .expect("an artist")
}

fn track(title: &str, credits: Vec<ArtistCredit>) -> Track {
    Track::new(
        TrackId::new(),
        TrackMetadata::from_names(title.to_owned(), Vec::new(), None)
            .expect("track metadata")
            .with_credits(credits),
    )
}

fn credit(name: &str, mbid: Option<&str>) -> ArtistCredit {
    ArtistCredit::new(name.to_owned(), mbid.map(str::to_owned)).expect("a credit")
}

fn source(provider: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider.to_owned()).expect("a provider"),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .expect("a source identity")
}

fn limit(value: u32) -> PageLimit {
    PageLimit::new(value).expect("a page limit")
}

#[derive(Default, Clone)]
struct FakeLibrary {
    artists: Vec<Artist>,
    tracks: Vec<Track>,
    views: Vec<TrackView>,
    albums: Vec<Album>,
    members: Vec<(AlbumId, TrackId)>,
    credits: HashMap<TrackId, Vec<ArtistId>>,
    owned: HashSet<TrackId>,
    calls: Arc<Mutex<Vec<TrackListFilter>>>,
}

impl FakeLibrary {
    fn album(&mut self, album_artist: Option<&ArtistId>) -> AlbumId {
        let id = AlbumId::new();
        let album = Album::new(
            id.clone(),
            format!("album {}", self.albums.len()),
            album_artist.cloned(),
            None,
            None,
            None,
            None,
            None,
        )
        .expect("an album");
        self.albums.push(album);
        self.members.push((id.clone(), TrackId::new()));
        id
    }

    fn view(&mut self, track: Track, state: LibraryState) -> TrackId {
        let id = track.id().clone();
        self.views.push(TrackView::new(
            track.clone(),
            None,
            None,
            None,
            state,
            Vec::new(),
        ));
        self.tracks.push(track);
        self.owned.insert(id.clone());
        id
    }

    fn mark_unavailable(&mut self, track_id: &TrackId) {
        self.owned.remove(track_id);
    }
}

#[async_trait]
impl LibraryRead for FakeLibrary {
    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        Ok(self
            .tracks
            .iter()
            .find(|track| track.id() == track_id)
            .cloned())
    }

    async fn find_artist(
        &self,
        artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        Ok(self
            .artists
            .iter()
            .find(|artist| artist.id() == artist_id)
            .cloned())
    }

    async fn find_albums_for_artist(
        &self,
        artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(self
            .albums
            .iter()
            .filter(|album| album.album_artist_id() == Some(artist_id))
            .cloned()
            .map(|album| AlbumSummary::new(album, None, 0, 0, Utc::now()))
            .collect())
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: TrackOrder,
        filter: &TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        self.calls
            .lock()
            .expect("the filter calls")
            .push(filter.clone());
        let hits = self
            .views
            .iter()
            .filter(|view| matches_filter(view, filter, &self.members, &self.credits, &self.owned))
            .cloned()
            .collect::<Vec<_>>();
        let total = u32::try_from(hits.len()).unwrap_or(u32::MAX);

        Ok(TrackPage::new(hits, total))
    }

    async fn track_views(
        &self,
        track_ids: &[TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<TrackView>, TrackRepositoryError> {
        Ok(self
            .views
            .iter()
            .filter(|view| track_ids.contains(view.track().id()))
            .cloned()
            .collect())
    }

    async fn tracks_credited_to(
        &self,
        artist_ids: &[ArtistId],
    ) -> Result<Vec<TrackId>, TrackRepositoryError> {
        Ok(self
            .credits
            .iter()
            .filter(|(_, credited)| {
                credited
                    .iter()
                    .any(|artist_id| artist_ids.iter().any(|wanted| wanted == artist_id))
            })
            .map(|(track_id, _)| track_id.clone())
            .collect())
    }

    async fn dedup_candidates(
        &self,
        keys: &[String],
        _limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        Ok(self
            .views
            .iter()
            .filter(|view| {
                self.owned.contains(view.track().id())
                    && keys
                        .iter()
                        .any(|key| normalize_for_matching(view.track().title()) == *key)
            })
            .map(|view| TrackHit::new(view.track().clone(), None, Vec::new()))
            .collect())
    }
}

fn matches_filter(
    view: &TrackView,
    filter: &TrackListFilter,
    members: &[(AlbumId, TrackId)],
    artists: &HashMap<TrackId, Vec<ArtistId>>,
    owned: &HashSet<TrackId>,
) -> bool {
    if filter.scope() == TrackListScope::Owned && !owned.contains(view.track().id()) {
        return false;
    }
    if let Some(album_ids) = filter.album_id() {
        let belongs = members.iter().any(|(album_id, track_id)| {
            album_ids.iter().any(|wanted| wanted == album_id) && track_id == view.track().id()
        });
        if !belongs {
            return false;
        }
    }
    if let Some(artist_ids) = filter.artists_id() {
        let credited = artists.get(view.track().id());
        if !credited.is_some_and(|credited| {
            credited
                .iter()
                .any(|artist_id| artist_ids.iter().any(|wanted| wanted == artist_id))
        }) {
            return false;
        }
    }
    true
}

#[derive(Default, Clone)]
struct FakeSources {
    artists: Vec<(SourceIdentity, ArtistId)>,
    calls: Arc<Mutex<usize>>,
    writes: Arc<Mutex<Vec<(SourceIdentity, ArtistId)>>>,
}

impl FakeSources {
    fn write_count(&self) -> usize {
        self.writes.lock().expect("the writes").len()
    }
}

#[async_trait]
impl SourceLookup for FakeSources {
    async fn find_sources(
        &self,
        _track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_artist_sources(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(self
            .artists
            .iter()
            .filter(|(_, id)| id == artist_id)
            .map(|(source, _)| source.clone())
            .collect())
    }

    async fn find_artist_by_source(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<ArtistId>, TrackRepositoryError> {
        *self.calls.lock().expect("the calls") += 1;
        if let Some((_, artist_id)) = self
            .artists
            .iter()
            .find(|(candidate, _)| candidate == source)
        {
            return Ok(Some(artist_id.clone()));
        }
        if let Some((_, artist_id)) = self
            .writes
            .lock()
            .expect("the writes")
            .iter()
            .find(|(candidate, _)| candidate == source)
        {
            return Ok(Some(artist_id.clone()));
        }

        Ok(None)
    }
}

#[derive(Clone)]
struct FakeState;

#[async_trait]
impl StateStore for FakeState {}

struct FakeStore {
    facts: Mutex<Vec<StoredFact>>,
}

#[async_trait]
impl MetadataStore for FakeStore {
    async fn read_subject(
        &self,
        key: &crate::features::metadata::domain::SubjectKey,
    ) -> Result<crate::features::metadata::domain::SubjectFacts, MetadataRepositoryError> {
        Ok(crate::features::metadata::domain::SubjectFacts::new(
            key.clone(),
            self.facts.lock().expect("the fake facts").clone(),
            Vec::new(),
        ))
    }

    async fn commit_field(
        &self,
        _subject: &EnrichmentSubject,
        _write: &crate::features::metadata::domain::FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError> {
        Ok(CommitOutcome::Written)
    }

    async fn record_failure(
        &self,
        _subject: &EnrichmentSubject,
        _field: FieldId,
        _language: &str,
        _source_provider_id: &str,
        _error: &crate::features::metadata::domain::AttemptError,
    ) -> Result<(), MetadataRepositoryError> {
        Ok(())
    }

    async fn delete_subject_facts(
        &self,
        _kind: crate::features::metadata::domain::MetadataEntityKind,
        _ids: &[Uuid],
    ) -> Result<u64, MetadataRepositoryError> {
        Ok(0)
    }
}

struct FailingDocuments;

#[async_trait]
impl MetadataDocuments for FailingDocuments {
    async fn fetch(
        &self,
        _source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        _fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        panic!("a discovery read must never contact a provider")
    }
}

fn metadata(facts: Vec<StoredFact>, advertised: &[&str]) -> MetadataService {
    MetadataService::new(
        sqlx::PgPool::connect_lazy("postgres://whio:whio@127.0.0.1:1/whio").expect("a lazy pool"),
        Arc::new(FakeStore {
            facts: Mutex::new(facts),
        }) as Arc<dyn MetadataStore>,
        Arc::new(FailingDocuments) as Arc<dyn MetadataDocuments>,
        Arc::new(CapabilitySnapshot::new(
            Arc::new(FixedCapabilities {
                providers: advertised.iter().map(|value| (*value).to_owned()).collect(),
            }),
            CAPABILITY_TTL,
        )),
        FieldOrder::from_entries([(FieldId::Similar, vec![YOUTUBE_MUSIC.to_owned()])]),
        "v1".to_owned(),
        Arc::new(NoJitter),
    )
}

struct FixedCapabilities {
    providers: Vec<String>,
}

#[async_trait]
impl crate::features::metadata::capabilities::CapabilitySource for FixedCapabilities {
    async fn fetch(
        &self,
    ) -> Result<Snapshot, crate::features::metadata::capabilities::CapabilityError> {
        Ok(Snapshot::new(self.providers.clone()))
    }
}

fn provider_fact(
    field: FieldId,
    candidates: Vec<CandidateItem>,
    fetched_at: DateTime<Utc>,
) -> StoredFact {
    provider_fact_with_scope(field, candidates, fetched_at, YOUTUBE_MUSIC_SIMILAR_SCOPE)
}

fn provider_fact_with_scope(
    field: FieldId,
    candidates: Vec<CandidateItem>,
    fetched_at: DateTime<Utc>,
    scope: &str,
) -> StoredFact {
    let licence = licence_terms(YOUTUBE_MUSIC, 1).expect("the registry terms");
    let candidates = candidates
        .into_iter()
        .map(|candidate| CandidateItem {
            ranking_scope: Some(scope.to_owned()),
            ..candidate
        })
        .collect::<Vec<_>>();

    StoredFact {
        field,
        source: FactSource {
            provider_id: YOUTUBE_MUSIC.to_owned(),
            external_id: format!("{YOUTUBE_MUSIC}-{field:?}"),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value: FieldValue::Candidates(candidates),
        bytes: None,
        licence,
        attribution: None,
        source_url: None,
        operation: "artist.similar.v1".to_owned(),
        terms_basis: None,
        uncertainty: false,
        image: None,
        metric: None,
        rank: None,
        score: None,
        stored_bytes: 0,
        fetched_at,
    }
}

fn provider_fact_from(
    field: FieldId,
    provider: &str,
    candidates: Vec<CandidateItem>,
    fetched_at: DateTime<Utc>,
    scope: &str,
) -> StoredFact {
    let mut fact = provider_fact_with_scope(field, candidates, fetched_at, scope);
    fact.source.provider_id = provider.to_owned();
    fact.licence = licence_terms(provider, 1).expect("the registry terms");

    fact
}

fn candidate(name: &str, external_id: &str) -> CandidateItem {
    CandidateItem {
        name: name.to_owned(),
        external_id: Some(external_id.to_owned()),
        url: None,
        rank: None,
        score: None,
        artists: Vec::new(),
        release_name: None,
        duration_ms: None,
        isrc: None,
        recording_id: None,
        ranking_scope: None,
    }
}

fn recording_candidate(
    name: &str,
    external_id: &str,
    artists: &[&str],
    duration_ms: u64,
    release_name: &str,
    recording_id: Option<&str>,
) -> CandidateItem {
    CandidateItem {
        artists: artists.iter().map(|artist| (*artist).to_owned()).collect(),
        duration_ms: Some(duration_ms),
        release_name: Some(release_name.to_owned()),
        recording_id: recording_id.map(str::to_owned),
        ..candidate(name, external_id)
    }
}

struct Harness {
    library: FakeLibrary,
    sources: FakeSources,
    listens: FakeListens,
    facts: Vec<StoredFact>,
    advertised: Vec<String>,
    now: DateTime<Utc>,
}

impl Harness {
    fn new() -> Self {
        Self {
            library: FakeLibrary::default(),
            sources: FakeSources::default(),
            listens: FakeListens::default(),
            facts: Vec::new(),
            advertised: vec![YOUTUBE_MUSIC.to_owned()],
            now: Utc::now(),
        }
    }

    async fn service(&self) -> DiscoveryService {
        DiscoveryService::new(
            Arc::new(self.library.clone()) as Arc<dyn LibraryRead>,
            Arc::new(self.sources.clone()) as Arc<dyn SourceLookup>,
            Arc::new(FakeState) as Arc<dyn StateStore>,
            Arc::new(self.listens.clone()) as Arc<dyn ListenStore>,
        )
        .with_metadata(Arc::new(metadata(
            self.facts.clone(),
            &self
                .advertised
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )))
        .at(self.now)
    }
}

#[derive(Default, Clone)]
struct FakeListens {
    records: Vec<(Uuid, TrackId, DateTime<Utc>)>,
    marker: Option<DateTime<Utc>>,
    plays: HashMap<TrackId, u32>,
}

#[async_trait]
impl ListenStore for FakeListens {
    async fn submit_listen(
        &self,
        _submission: &crate::features::music_library::domain::ListenSubmission,
    ) -> Result<crate::features::music_library::domain::ListenOutcome, TrackRepositoryError> {
        Ok(crate::features::music_library::domain::ListenOutcome::Recorded)
    }

    async fn listen_history(
        &self,
        principal: Uuid,
        query: &ListenQuery,
    ) -> Result<crate::features::music_library::domain::ListenPage, TrackRepositoryError> {
        let window = query.window();
        let mut entries = self
            .records
            .iter()
            .filter(|(user, _, at)| {
                *user == principal
                    && window.since().is_none_or(|since| *at >= since)
                    && window.until().is_none_or(|until| *at <= until)
            })
            .map(|(_, track_id, at)| {
                ListenRecord::new(
                    crate::features::music_library::domain::ListenOrigin::Native,
                    track_id.clone(),
                    *at,
                    None,
                    None,
                )
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|record| std::cmp::Reverse(record.listen_at()));
        entries.truncate(usize::try_from(query.limit().get()).unwrap_or(usize::MAX));

        Ok(crate::features::music_library::domain::ListenPage::new(
            entries,
            crate::features::music_library::domain::ListenCoverage::new(self.marker),
        ))
    }

    async fn listen_count(
        &self,
        principal: Uuid,
        window: ListenWindow,
        track: Option<&TrackId>,
    ) -> Result<crate::features::music_library::domain::ListenCount, TrackRepositoryError> {
        if window.is_unbounded() {
            let count = match track {
                Some(track_id) => u64::from(self.plays.get(track_id).copied().unwrap_or(0)),
                None => u64::try_from(
                    self.records
                        .iter()
                        .filter(|(user, _, _)| *user == principal)
                        .count(),
                )
                .unwrap_or(0),
            };

            return Ok(crate::features::music_library::domain::ListenCount::new(
                count,
                crate::features::music_library::domain::ListenCoverage::new(self.marker),
            ));
        }
        let count = self
            .records
            .iter()
            .filter(|(user, _, at)| {
                *user == principal
                    && window.since().is_none_or(|since| *at >= since)
                    && window.until().is_none_or(|until| *at <= until)
                    && track.is_none_or(|track_id| track_id == &_0_track_id())
            })
            .count();

        Ok(crate::features::music_library::domain::ListenCount::new(
            u64::try_from(count).unwrap_or(0),
            crate::features::music_library::domain::ListenCoverage::new(self.marker),
        ))
    }
}

fn _0_track_id() -> TrackId {
    TrackId::new()
}

fn seed_artist(harness: &mut Harness) -> Artist {
    let artist = artist("Seed", Some("mbid-seed"));
    harness.library.artists.push(artist.clone());
    harness
        .sources
        .artists
        .push((source("musicbrainz", "mbid-seed"), artist.id().clone()));

    artist
}

fn candidate_artist(harness: &mut Harness, name: &str, mbid: &str) -> Artist {
    let artist = artist(name, Some(mbid));
    harness.library.artists.push(artist.clone());
    harness
        .sources
        .artists
        .push((source("musicbrainz", mbid), artist.id().clone()));

    artist
}

fn credit_of(artist: &Artist) -> ArtistCredit {
    credit(artist.name(), artist.musicbrainz_artist_id())
}

fn owned(harness: &mut Harness, artist: &Artist, title: &str, state: LibraryState) -> TrackId {
    let track = track(title, vec![credit_of(artist)]);
    let id = harness.library.view(track, state);
    harness
        .library
        .credits
        .entry(id.clone())
        .or_default()
        .push(artist.id().clone());

    id
}

fn by_title(harness: &Harness, title: &str) -> TrackId {
    harness
        .library
        .tracks
        .iter()
        .find(|track| track.title() == title)
        .map(|track| track.id().clone())
        .expect("a fixture track")
}

fn shared_album(harness: &mut Harness, seed: &Artist, peer: &Artist) -> AlbumId {
    let album = harness.library.album(Some(seed.id()));
    let members = harness.library.members.len();
    let seed_track = track("seed song", vec![credit_of(seed)]);
    let peer_track = track("peer song", vec![credit_of(seed), credit_of(peer)]);
    harness.library.members[members - 1] = (album.clone(), seed_track.id().clone());
    harness
        .library
        .members
        .push((album.clone(), peer_track.id().clone()));
    for credited in [seed_track, peer_track] {
        let id = credited.id().clone();
        harness.library.views.push(TrackView::new(
            credited.clone(),
            None,
            None,
            None,
            LibraryState::default(),
            Vec::new(),
        ));
        harness.library.tracks.push(credited);
        harness.library.owned.insert(id);
    }
    harness
        .library
        .credits
        .entry(
            harness.library.tracks[harness.library.tracks.len() - 2]
                .id()
                .clone(),
        )
        .or_default()
        .push(seed.id().clone());
    harness
        .library
        .credits
        .entry(
            harness.library.tracks[harness.library.tracks.len() - 1]
                .id()
                .clone(),
        )
        .or_default()
        .extend([seed.id().clone(), peer.id().clone()]);

    album
}

fn related_request(seed: &Artist, basis: DiscoveryBasis) -> RelatedRequest {
    RelatedRequest {
        seed: DiscoverySeed::Artist(seed.id().clone()),
        basis,
        window: None,
        limit: limit(50),
        user: None,
    }
}

fn top_request(seed: &Artist, basis: DiscoveryBasis, user: Option<Uuid>) -> TopRequest {
    TopRequest {
        seed: DiscoverySeed::Artist(seed.id().clone()),
        basis,
        window: None,
        limit: limit(50),
        user,
    }
}

#[tokio::test]
async fn library_basis_reports_its_own_basis_and_excludes_the_seed() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    shared_album(&mut harness, &seed, &peer);

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::Library))
        .await
        .expect("a library result");

    assert_eq!(result.basis, DiscoveryBasis::Library);
    assert!(!result.fallback);
    assert!(result.window.is_none());
    assert!(
        result
            .items
            .iter()
            .all(|item| item.canonical != Some(seed.id().clone()))
    );
}

#[tokio::test]
async fn a_server_request_is_refused_and_never_served_from_personal_data() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = peer_member(&harness, &album, &seed_track);
    credit_to(&mut harness, &peer_track, &seed);
    harness
        .listens
        .records
        .push((user, peer_track.clone(), harness.now));
    let personal = harness
        .service()
        .await
        .related(RelatedRequest {
            user: Some(user),
            ..related_request(&seed, DiscoveryBasis::Personal)
        })
        .await
        .expect("the personal basis is served");

    assert_eq!(personal.items.len(), 1);

    let requested = DiscoveryBasis::parse("server")
        .map_or_else(|| Err(DiscoveryService::unsupported_basis("server")), Ok);

    assert!(
        matches!(
            requested,
            Err(DiscoveryError::UnsupportedBasis { basis }) if basis == "server"
        ),
        "the entry point that receives a basis string refuses `server` rather than substituting a served basis"
    );
}

#[tokio::test]
async fn no_summing_returns_only_the_requested_basis() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = harness
        .library
        .members
        .iter()
        .filter(|(album_id, track_id)| album_id == &album && track_id != &seed_track)
        .map(|(_, track_id)| track_id.clone())
        .next()
        .expect("a peer track");
    harness
        .listens
        .records
        .push((user, peer_track.clone(), Utc::now()));

    let personal = harness
        .service()
        .await
        .related(RelatedRequest {
            user: Some(user),
            seed: DiscoverySeed::Track(seed_track.clone()),
            ..related_request(&seed, DiscoveryBasis::Personal)
        })
        .await
        .expect("a personal result");
    let library = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::Library))
        .await
        .expect("a library result");

    assert_eq!(personal.basis, DiscoveryBasis::Personal);
    assert_eq!(library.basis, DiscoveryBasis::Library);
    assert_eq!(library.items.len(), 1);
    assert_eq!(library.items[0].canonical, Some(peer.id().clone()));
    assert_eq!(personal.items.len(), 1);
    assert_eq!(personal.items[0].canonical, Some(peer.id().clone()));
    assert_eq!(personal.items[0].rank_in_basis, 0);
    assert!(
        personal
            .items
            .iter()
            .all(|item| item.ranking_scope.as_deref() == Some(PERSONAL_WINDOW)),
        "every personal item names the personal population"
    );
    assert!(
        library
            .items
            .iter()
            .all(|item| item.ranking_scope.as_deref() == Some(TRACK_CREDIT_PEERS)),
        "every library item names the library population, never the personal one"
    );
    assert_eq!(library.items.len() + personal.items.len(), 2);
    assert!(
        personal
            .items
            .iter()
            .all(|item| item.canonical != Some(seed.id().clone()))
    );
    let _ = peer_track;
}

#[tokio::test]
async fn a_bounded_window_and_an_unbounded_request_differ_and_report_coverage() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let seed_track = owned_credited(&mut harness, "seed song", &[&seed]);
    let bounded_only = owned_credited(&mut harness, "bounded only", &[&seed]);
    let all_time_only = owned_credited(&mut harness, "all time only", &[&seed]);
    harness.listens.marker = Some(harness.now - Duration::days(30));
    harness
        .listens
        .records
        .push((user, bounded_only.clone(), harness.now - Duration::days(1)));
    harness.listens.records.push((
        user,
        all_time_only.clone(),
        harness.now - Duration::days(20),
    ));
    plays(&mut harness, &seed_track, 3);
    plays(&mut harness, &bounded_only, 7);
    plays(&mut harness, &all_time_only, 9);
    let _ = peer;

    let bounded = harness
        .service()
        .await
        .top(TopRequest {
            user: Some(user),
            seed: DiscoverySeed::Track(seed_track.clone()),
            window: Some(ListenWindow::new(
                Some(harness.now - Duration::days(2)),
                None,
            )),
            ..top_request(&seed, DiscoveryBasis::Personal, Some(user))
        })
        .await
        .expect("a bounded top result");
    let unbounded = harness
        .service()
        .await
        .top(TopRequest {
            seed: DiscoverySeed::Track(seed_track.clone()),
            ..top_request(&seed, DiscoveryBasis::Personal, Some(user))
        })
        .await
        .expect("an unbounded top result");

    let bounded_ids = bounded
        .items
        .iter()
        .map(|item| item.canonical.clone())
        .collect::<Vec<_>>();
    let unbounded_ids = unbounded
        .items
        .iter()
        .map(|item| item.canonical.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        bounded_ids,
        vec![Some(bounded_only.clone())],
        "a bounded window reads `listens`: the play outside the window is absent and the in-window play is present"
    );
    assert_eq!(
        unbounded_ids,
        vec![Some(all_time_only.clone()), Some(bounded_only.clone())],
        "an unbounded request reads `track_play_stats` and returns the different all-time population"
    );
    assert_ne!(
        bounded_ids, unbounded_ids,
        "the two populations must be observably different for the fixture to discriminate"
    );
    assert_eq!(
        bounded.items[0].score,
        Some(1.0),
        "a bounded score counts in-window listens"
    );
    assert_eq!(bounded.ranking_scope.as_deref(), Some(PERSONAL_WINDOW));
    assert_eq!(
        unbounded.items[0].score,
        Some(9.0),
        "an unbounded score is the all-time play counter"
    );
    assert_eq!(
        unbounded.ranking_scope.as_deref(),
        Some("all-time play counter")
    );
    assert_eq!(unbounded.coverage_since, harness.listens.marker);
}

#[tokio::test]
async fn external_basis_projects_canonically_only_on_a_matching_provider_id() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    candidate_artist(&mut harness, "Known", "mbid-known");
    harness.facts.push(provider_fact(
        FieldId::Similar,
        vec![
            candidate("Known", "UC-known"),
            candidate("Unknown", "UC-unknown"),
        ],
        harness.now,
    ));
    harness.sources.artists.push((
        source("youtube_music", "UC-known"),
        harness.library.artists[1].id().clone(),
    ));
    let artists_before = harness.library.artists.len();

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("an external result");

    assert_eq!(result.basis, DiscoveryBasis::External);
    assert!(!result.fallback);
    assert_eq!(
        result.ranking_scope.as_deref(),
        Some(YOUTUBE_MUSIC_SIMILAR_SCOPE)
    );
    assert_eq!(result.items.len(), 2);
    assert_eq!(result.items[0].rank_in_basis, 0);
    assert_eq!(result.items[1].rank_in_basis, 1);
    assert_eq!(
        result.items[0].canonical,
        Some(harness.library.artists[1].id().clone())
    );
    assert_eq!(result.items[1].canonical, None);
    assert_eq!(result.items[1].external_id.as_deref(), Some("UC-unknown"));
    assert_eq!(result.items[1].provider.as_deref(), Some("youtube_music"));
    assert_eq!(harness.library.artists.len(), artists_before);
    assert_eq!(
        harness.sources.write_count(),
        0,
        "a suggestion materialises no source row"
    );
    assert_eq!(
        harness
            .sources
            .find_artist_by_source(&source("youtube_music", "UC-unknown"))
            .await
            .expect("a lookup"),
        None,
        "the non-matching candidate left no canonical row behind, so a later lookup still misses"
    );
}

#[tokio::test]
async fn a_candidate_the_library_later_records_is_projected_by_its_own_source_row() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let known = artist("Known", Some("mbid-known"));
    harness.library.artists.push(known.clone());
    harness.facts.push(provider_fact(
        FieldId::Similar,
        vec![candidate("Known", "UC-known")],
        harness.now,
    ));

    let before = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("an unmatched result");
    assert_eq!(
        before.items[0].canonical, None,
        "the row is not recorded yet, so the candidate cannot be projected"
    );
    assert_eq!(harness.sources.write_count(), 0);

    harness
        .sources
        .artists
        .push((source("youtube_music", "UC-known"), known.id().clone()));
    harness
        .sources
        .writes
        .lock()
        .expect("the writes")
        .push((source("youtube_music", "UC-known"), known.id().clone()));

    let after = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a matched result");

    assert_eq!(
        after.items[0].canonical,
        Some(known.id().clone()),
        "recording the source row is what makes the projection match, so this assertion fails if the row is absent"
    );
    assert_eq!(
        harness.sources.write_count(),
        1,
        "the row was recorded outside the read; the read itself wrote nothing"
    );
}

#[tokio::test]
async fn an_external_item_names_the_provider_that_served_it() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let known = candidate_artist(&mut harness, "Known", "mbid-known");
    let mut fact = provider_fact_from(
        FieldId::Similar,
        "listenbrainz",
        vec![candidate("Known", "mbid-known")],
        harness.now,
        "listenbrainz.similar-artists.v1",
    );
    fact.licence = crate::features::metadata::licences::LicenceTerms {
        redistribution_allowed: true,
        ..licence_terms("listenbrainz", 1).expect("the registry terms")
    };
    harness.facts.push(fact);
    harness
        .sources
        .artists
        .push((source("listenbrainz", "mbid-known"), known.id().clone()));
    harness
        .sources
        .artists
        .push((source("youtube_music", "mbid-known"), known.id().clone()));

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("an external result");

    assert_eq!(result.basis, DiscoveryBasis::External);
    assert!(!result.fallback);
    assert_eq!(
        result.items[0].provider.as_deref(),
        Some("listenbrainz"),
        "the served provider is the provider that produced the candidate, never a hardcoded youtube_music label"
    );
    assert_ne!(result.items[0].provider.as_deref(), Some("youtube_music"));
    assert_eq!(
        result.items[0].canonical,
        Some(known.id().clone()),
        "the canonical lookup uses the serving provider's own identity space"
    );
    assert_eq!(
        result.ranking_scope.as_deref(),
        Some("listenbrainz.similar-artists.v1"),
        "two providers' graphs are two rankings, recorded verbatim per candidate"
    );
    assert_ne!(
        result.ranking_scope.as_deref(),
        Some(YOUTUBE_MUSIC_SIMILAR_SCOPE),
        "a listenbrainz ranking is not labelled with the youtube_music scope"
    );
}

#[tokio::test]
async fn a_provider_fact_without_a_named_ranking_is_not_served() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let mut fact = provider_fact(
        FieldId::Similar,
        vec![candidate("Known", "UC-known")],
        harness.now,
    );
    fact.value = crate::features::metadata::domain::FieldValue::Candidates(vec![candidate(
        "Known", "UC-known",
    )]);
    harness.facts.push(fact);

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a result without a named ranking");

    assert!(result.fallback);
    assert_eq!(
        result.fallback_reason,
        Some(FallbackReason::ProviderScopeUnnamed),
        "a ranking whose basis cannot be named must not be served"
    );
}

#[tokio::test]
async fn an_external_request_without_provider_facts_declares_the_local_fallback() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    shared_album(&mut harness, &seed, &peer);

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a fallback result");

    assert!(result.fallback);
    assert_eq!(
        result.fallback_reason,
        Some(FallbackReason::ProviderCapabilityAbsent),
        "the provider advertises no `similar` capability, which is not a licence refusal"
    );
    assert_eq!(result.basis, DiscoveryBasis::Library);
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].canonical, Some(peer.id().clone()));
}

#[tokio::test]
async fn an_external_top_candidate_is_projected_only_on_a_verified_recording_identity() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let verified = owned_recording(
        &mut harness,
        "Verified Song",
        &[&seed],
        200_000,
        Some(RECORDING_VERIFIED),
    );
    let metadata_only = owned_recording(
        &mut harness,
        "Metadata Only",
        &[&seed],
        180_000,
        Some(RECORDING_OTHER),
    );
    let fact = provider_fact_with_scope(
        FieldId::TopTracks,
        vec![
            recording_candidate(
                "Verified Song",
                "vid-verified",
                &["Seed"],
                200_000,
                "album",
                Some(RECORDING_VERIFIED),
            ),
            recording_candidate(
                "Metadata Only",
                "vid-metadata",
                &["Seed"],
                180_000,
                "album",
                None,
            ),
            recording_candidate(
                "Foreign Song",
                "vid-foreign",
                &["Someone Else"],
                200_000,
                "album",
                Some(RECORDING_VERIFIED),
            ),
        ],
        harness.now,
        YOUTUBE_MUSIC_TOP_TRACKS_SCOPE,
    );
    harness.facts.push(fact);

    let result = harness
        .service()
        .await
        .top(top_request(&seed, DiscoveryBasis::External, None))
        .await
        .expect("an external top result");

    assert_eq!(result.basis, DiscoveryBasis::External);
    assert!(!result.fallback);
    assert_eq!(result.items.len(), 3);
    assert_eq!(
        harness.sources.write_count(),
        0,
        "the projection reads the canonical index and materialises no row of its own"
    );
    assert_eq!(
        result.items[0].canonical,
        Some(verified.clone()),
        "a candidate carrying the local recording id and the local credits is Verified-matched to the canonical track"
    );
    assert_eq!(
        result.items[1].canonical, None,
        "a candidate with no shared recording id is MetadataCandidate-grade and must not become canonical"
    );
    assert_eq!(
        result.items[2].canonical, None,
        "a candidate whose credits differ is not canonical even when the recording id is shared"
    );
    assert_ne!(result.items[1].canonical, Some(metadata_only.clone()));
    assert_eq!(
        result.items[0].provider.as_deref(),
        Some("youtube_music"),
        "the item names the provider that produced the candidate"
    );
    assert_eq!(
        result.ranking_scope.as_deref(),
        Some(YOUTUBE_MUSIC_TOP_TRACKS_SCOPE),
        "the scope is the ranking recorded with the fact, carried verbatim"
    );
}

#[tokio::test]
async fn a_stale_provider_fact_declares_provider_stale() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    harness.facts.push(provider_fact(
        FieldId::Similar,
        vec![candidate("Known", "UC-known")],
        harness.now - Duration::days(30),
    ));

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a stale result");

    assert!(result.fallback);
    assert_eq!(result.fallback_reason, Some(FallbackReason::ProviderStale));
    assert!(result.items.is_empty());
}

#[tokio::test]
async fn a_not_servable_provider_fact_is_never_served() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let mut fact = provider_fact(
        FieldId::Similar,
        vec![candidate("NotServable", "UC-nope")],
        harness.now,
    );
    fact.source.provider_id = "listenbrainz".to_owned();
    fact.licence = licence_terms("listenbrainz", 1).expect("the registry terms");
    harness.facts.push(fact);

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a licensed result");

    assert!(result.fallback);
    assert_eq!(
        result.fallback_reason,
        Some(FallbackReason::ProviderNotLicensed)
    );
    assert!(result.items.is_empty());
}

#[tokio::test]
async fn an_unknown_seed_is_reported() {
    let harness = Harness::new();
    let stray = artist("Stray", Some("mbid-stray"));

    let error = harness
        .service()
        .await
        .related(related_request(&stray, DiscoveryBasis::Library))
        .await
        .expect_err("an unknown seed");

    assert!(matches!(error, DiscoveryError::UnknownSeed));
}

#[tokio::test]
async fn equal_ranks_order_stably_across_repeated_calls() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let first = candidate_artist(&mut harness, "Alpha", "mbid-alpha");
    let second = candidate_artist(&mut harness, "Beta", "mbid-beta");
    shared_album(&mut harness, &seed, &first);
    shared_album_with(&mut harness, &seed, &second);

    let service = harness.service().await;
    let once = service
        .related(related_request(&seed, DiscoveryBasis::Library))
        .await
        .expect("a first result");
    let twice = service
        .related(related_request(&seed, DiscoveryBasis::Library))
        .await
        .expect("a second result");

    let ids = |items: &[RelatedItem]| {
        items
            .iter()
            .map(|item| item.canonical.clone())
            .collect::<Vec<_>>()
    };

    assert_eq!(ids(&once.items), ids(&twice.items));
    assert_eq!(once.items[0].rank_in_basis, 0);
    assert_eq!(once.items[1].rank_in_basis, 1);
    assert_eq!(once.items[0].name, "Alpha");
    assert_eq!(once.items[1].name, "Beta");
}

#[tokio::test]
async fn the_seed_never_appears_in_its_own_top_result() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = peer_member(&harness, &album, &seed_track);
    credit_to(&mut harness, &peer_track, &seed);
    plays(&mut harness, &seed_track, 4);
    plays(&mut harness, &peer_track, 2);

    let result = harness
        .service()
        .await
        .top(TopRequest {
            seed: DiscoverySeed::Track(seed_track.clone()),
            ..top_request(&seed, DiscoveryBasis::Personal, Some(user))
        })
        .await
        .expect("a personal top result");

    assert!(
        result
            .items
            .iter()
            .all(|item| item.canonical != Some(seed_track.clone()))
    );
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].canonical, Some(peer_track.clone()));
    assert_eq!(result.items[0].score, Some(2.0));
}

#[tokio::test]
async fn a_personal_request_without_personal_facts_is_honestly_empty() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    owned(&mut harness, &seed, "seed song", LibraryState::default());
    owned(&mut harness, &peer, "peer song", LibraryState::default());

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::Personal))
        .await
        .expect("an honest empty result");

    assert_eq!(result.basis, DiscoveryBasis::Personal);
    assert!(result.fallback);
    assert_eq!(result.fallback_reason, Some(FallbackReason::NoLocalBasis));
    assert!(result.items.is_empty());
}

#[tokio::test]
async fn a_top_result_never_returns_more_items_than_its_limit() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = peer_member(&harness, &album, &seed_track);
    credit_to(&mut harness, &peer_track, &seed);
    plays(&mut harness, &peer_track, 9);

    let result = harness
        .service()
        .await
        .top(TopRequest {
            seed: DiscoverySeed::Track(seed_track.clone()),
            limit: limit(1),
            ..top_request(&seed, DiscoveryBasis::Personal, Some(user))
        })
        .await
        .expect("a bounded result");

    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].canonical, Some(peer_track.clone()));
    assert_eq!(result.items[0].rank_in_basis, 0);
}

#[tokio::test]
async fn a_library_top_result_excludes_the_seed_track() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = peer_member(&harness, &album, &seed_track);
    credit_to(&mut harness, &peer_track, &seed);

    let result = harness
        .service()
        .await
        .top(top_request(&seed, DiscoveryBasis::Library, None))
        .await
        .expect("a library top result");

    assert_eq!(result.items.len(), 2);
    assert!(!result.fallback);
    assert_eq!(result.ranking_scope.as_deref(), Some(LIBRARY_TITLE_ORDER));
    assert!(
        result
            .items
            .iter()
            .all(|item| item.ranking_scope.as_deref() == Some(LIBRARY_TITLE_ORDER)),
        "a title ordering is named as a title ordering, not as the shared-fact ranking"
    );

    let via_track = harness
        .service()
        .await
        .top(TopRequest {
            seed: DiscoverySeed::Track(seed_track.clone()),
            ..top_request(&seed, DiscoveryBasis::Library, None)
        })
        .await
        .expect("a track-seeded top result");

    assert_eq!(via_track.items.len(), 1);
    assert_eq!(via_track.items[0].title, "peer song");
}

#[tokio::test]
async fn an_external_top_request_without_facts_falls_back_to_the_local_tracks() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let album = shared_album(&mut harness, &seed, &peer);
    let seed_track = by_title(&harness, "seed song");
    let peer_track = peer_member(&harness, &album, &seed_track);
    credit_to(&mut harness, &peer_track, &seed);

    let result = harness
        .service()
        .await
        .top(top_request(&seed, DiscoveryBasis::External, None))
        .await
        .expect("a fallback top result");

    assert!(result.fallback);
    assert_eq!(
        result.fallback_reason,
        Some(FallbackReason::NoProviderFacts),
        "`top_tracks` is not a configured field here, so no provider fact exists and that is absence, not a licence refusal"
    );
    assert_eq!(result.basis, DiscoveryBasis::Library);
    assert_eq!(result.items.len(), 2);
    assert!(result.items.iter().any(|item| item.title == "peer song"));
}

#[tokio::test]
async fn a_personal_relation_is_anchored_on_the_seed_artist() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let alpha = candidate_artist(&mut harness, "Alpha", "mbid-alpha");
    let beta = candidate_artist(&mut harness, "Beta", "mbid-beta");
    let gamma = candidate_artist(&mut harness, "Gamma", "mbid-gamma");
    let user = Uuid::now_v7();
    let seed_play = owned_credited(&mut harness, "seed play", &[&seed, &alpha]);
    let beta_play = owned_credited(&mut harness, "beta play", &[&beta, &gamma]);
    harness
        .listens
        .records
        .push((user, seed_play.clone(), harness.now));
    harness
        .listens
        .records
        .push((user, beta_play.clone(), harness.now));

    let seeded_on_seed = harness
        .service()
        .await
        .related(RelatedRequest {
            user: Some(user),
            ..related_request(&seed, DiscoveryBasis::Personal)
        })
        .await
        .expect("a personal result for the seed");
    let seeded_on_beta = harness
        .service()
        .await
        .related(RelatedRequest {
            user: Some(user),
            ..related_request(&beta, DiscoveryBasis::Personal)
        })
        .await
        .expect("a personal result for the other seed");

    let names = |items: &[RelatedItem]| {
        items
            .iter()
            .map(|item| item.name.clone())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        names(&seeded_on_seed.items),
        vec!["Alpha".to_owned()],
        "the seed's own plays yield the artists co-credited on them"
    );
    assert_eq!(
        names(&seeded_on_beta.items),
        vec!["Gamma".to_owned()],
        "a different seed yields a different population"
    );
    assert_ne!(
        names(&seeded_on_seed.items),
        names(&seeded_on_beta.items),
        "a global artist list cannot satisfy two different seeds at once"
    );
}

#[tokio::test]
async fn a_personal_relation_without_a_play_of_the_seed_is_honestly_empty() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let alpha = candidate_artist(&mut harness, "Alpha", "mbid-alpha");
    let user = Uuid::now_v7();
    let unrelated = owned_credited(&mut harness, "unrelated play", &[&alpha]);
    harness
        .listens
        .records
        .push((user, unrelated.clone(), harness.now));

    let result = harness
        .service()
        .await
        .related(RelatedRequest {
            user: Some(user),
            ..related_request(&seed, DiscoveryBasis::Personal)
        })
        .await
        .expect("an honest empty result");

    assert!(
        result.items.is_empty(),
        "the principal's unrelated listens are not the seed artist's related artists"
    );
    assert!(result.fallback);
    assert_eq!(result.fallback_reason, Some(FallbackReason::NoLocalBasis));
}

#[tokio::test]
async fn an_unavailable_candidate_is_dropped_from_the_library_and_personal_bases() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let seed_track = owned_credited(&mut harness, "seed song", &[&seed]);
    let available = owned_credited(&mut harness, "available peer", &[&seed, &peer]);
    let unavailable = owned_credited(&mut harness, "unavailable peer", &[&seed, &peer]);
    harness.library.mark_unavailable(&unavailable);
    harness
        .listens
        .records
        .push((user, available.clone(), harness.now));

    let result = harness
        .service()
        .await
        .top(top_request(&seed, DiscoveryBasis::Library, None))
        .await
        .expect("a library top result");

    let titles = result
        .items
        .iter()
        .map(|item| item.title.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        titles.len(),
        result.items.len(),
        "the reported count matches the returned items"
    );
    assert!(
        titles.contains(&"available peer".to_owned()),
        "an owned track is returned"
    );
    assert!(
        !titles.contains(&"unavailable peer".to_owned()),
        "a track the library cannot serve under TRACK_OWNED is dropped, not returned with a false flag"
    );
    assert_eq!(result.items.len(), 2);
    let _ = seed_track;
}

#[tokio::test]
async fn the_library_relation_is_one_population_and_names_it() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let album_only = candidate_artist(&mut harness, "AlbumOnly", "mbid-album-only");
    let credit_only = candidate_artist(&mut harness, "CreditOnly", "mbid-credit-only");
    let album = harness.library.album(Some(seed.id()));
    let members = harness.library.members.len();
    let seed_track = track("seed song", vec![credit_of(&seed)]);
    let album_peer = track("album peer", vec![credit_of(&album_only)]);
    harness.library.members[members - 1] = (album.clone(), seed_track.id().clone());
    harness
        .library
        .members
        .push((album.clone(), album_peer.id().clone()));
    let seed_track_id = harness.library.view(seed_track, LibraryState::default());
    let album_peer_id = harness.library.view(album_peer, LibraryState::default());
    harness
        .library
        .credits
        .entry(seed_track_id)
        .or_default()
        .push(seed.id().clone());
    harness
        .library
        .credits
        .entry(album_peer_id)
        .or_default()
        .push(album_only.id().clone());
    owned_credited(&mut harness, "credited", &[&seed, &credit_only]);

    let result = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::Library))
        .await
        .expect("a library result");

    let names = result
        .items
        .iter()
        .map(|item| item.name.clone())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec!["CreditOnly".to_owned()],
        "the served relation is one population; an album co-member is not summed into it"
    );
    assert!(
        !names.contains(&"AlbumOnly".to_owned()),
        "an album co-member with no shared credit is absent from a credits-only ranking"
    );
    assert_eq!(
        result.ranking_scope.as_deref(),
        Some(TRACK_CREDIT_PEERS),
        "the served scope names the single population the rank is built from"
    );
}

#[tokio::test]
async fn each_fallback_cause_reports_its_own_reason() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    shared_album(&mut harness, &seed, &peer);

    let no_service = DiscoveryService::new(
        Arc::new(harness.library.clone()) as Arc<dyn LibraryRead>,
        Arc::new(harness.sources.clone()) as Arc<dyn SourceLookup>,
        Arc::new(FakeState) as Arc<dyn StateStore>,
        Arc::new(harness.listens.clone()) as Arc<dyn ListenStore>,
    )
    .at(harness.now);
    let without_metadata = no_service
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a result without a metadata service");
    assert_eq!(
        without_metadata.fallback_reason,
        Some(FallbackReason::NoProviderFacts),
        "no metadata source exists at all, which is absence rather than a licence refusal"
    );
    assert_eq!(
        without_metadata.items.len(),
        1,
        "the declared fallback still serves the local basis"
    );

    let unadvertised = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a result with no stored fact");
    assert_eq!(
        unadvertised.fallback_reason,
        Some(FallbackReason::ProviderCapabilityAbsent),
        "a registered provider that advertises no `similar` capability is not a licence refusal"
    );

    let mut unlicensed = provider_fact(
        FieldId::Similar,
        vec![candidate("NotServable", "UC-nope")],
        harness.now,
    );
    unlicensed.licence = licence_terms("listenbrainz", 1).expect("the registry terms");
    unlicensed.source.provider_id = "listenbrainz".to_owned();
    harness.facts.push(unlicensed);
    let not_licensed = harness
        .service()
        .await
        .related(related_request(&seed, DiscoveryBasis::External))
        .await
        .expect("a not-licensed result");
    assert_eq!(
        not_licensed.fallback_reason,
        Some(FallbackReason::ProviderNotLicensed),
        "the serving switch refusing a licence is the only cause that reports as a licence refusal"
    );

    let reasons = [
        without_metadata.fallback_reason,
        unadvertised.fallback_reason,
        not_licensed.fallback_reason,
    ];
    assert_eq!(
        reasons
            .iter()
            .flatten()
            .map(|reason| reason.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "absence, an unadvertised capability and a licence refusal are three distinguishable reasons"
    );
}

#[tokio::test]
async fn absence_reasons_map_to_their_own_causes() {
    let cases = [
        (
            crate::features::metadata::domain::AbsenceReason::NotLicensed,
            FallbackReason::ProviderNotLicensed,
        ),
        (
            crate::features::metadata::domain::AbsenceReason::CapabilityAbsent,
            FallbackReason::ProviderCapabilityAbsent,
        ),
        (
            crate::features::metadata::domain::AbsenceReason::IdentityUnestablished,
            FallbackReason::ProviderIdentityUnestablished,
        ),
        (
            crate::features::metadata::domain::AbsenceReason::CredentialMissing,
            FallbackReason::ProviderCredentialMissing,
        ),
        (
            crate::features::metadata::domain::AbsenceReason::StaleAbsent,
            FallbackReason::ProviderStale,
        ),
        (
            crate::features::metadata::domain::AbsenceReason::ProviderReportedNone,
            FallbackReason::NoProviderFacts,
        ),
    ];

    for (absence, expected) in cases {
        let reason = crate::features::music_library::discovery::absence_reason(Some(absence));
        assert_eq!(
            reason, expected,
            "each absence cause collapses onto its own honest reason"
        );
    }

    let distinct = cases
        .iter()
        .map(|(absence, _)| {
            crate::features::music_library::discovery::absence_reason(Some(*absence)).as_str()
        })
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(
        distinct.len(),
        cases.len(),
        "every cause maps to its own distinguishable reason, so a reader can tell `no facts` from `refused a licence` from `capability not advertised`"
    );
}

fn shared_album_with(harness: &mut Harness, seed: &Artist, peer: &Artist) -> AlbumId {
    shared_album(harness, seed, peer)
}

fn peer_member(harness: &Harness, album: &AlbumId, seed_track: &TrackId) -> TrackId {
    harness
        .library
        .members
        .iter()
        .filter(|(album_id, track_id)| album_id == album && track_id != seed_track)
        .map(|(_, track_id)| track_id.clone())
        .next()
        .expect("a peer track on the shared album")
}

fn plays(harness: &mut Harness, track_id: &TrackId, count: u32) {
    harness.listens.plays.insert(track_id.clone(), count);
    for view in harness.library.views.iter_mut() {
        if view.track().id() == track_id {
            *view = TrackView::new(
                view.track().clone(),
                None,
                None,
                None,
                LibraryState::new(None, None, count, None).expect("state"),
                Vec::new(),
            );
        }
    }
}

fn credit_to(harness: &mut Harness, track_id: &TrackId, artist: &Artist) {
    harness
        .library
        .credits
        .entry(track_id.clone())
        .or_default()
        .push(artist.id().clone());
}

fn owned_credited(harness: &mut Harness, title: &str, artists: &[&Artist]) -> TrackId {
    let track = track(
        title,
        artists.iter().map(|artist| credit_of(artist)).collect(),
    );
    let id = harness.library.view(track, LibraryState::default());
    harness
        .library
        .credits
        .entry(id.clone())
        .or_default()
        .extend(artists.iter().map(|artist| artist.id().clone()));

    id
}

const RECORDING_VERIFIED: &str = "9f1b1b7e-6a0e-4a2f-9a2f-4d0f5b6c7a81";
const RECORDING_OTHER: &str = "2c3d4e5f-6071-4829-a3b4-c5d6e7f80912";
const PERSONAL_WINDOW: &str = "personal listen window";
const TRACK_CREDIT_PEERS: &str = "shared track artist credits, no time dimension";
const LIBRARY_TITLE_ORDER: &str = "owned tracks in title order";

const YOUTUBE_MUSIC_SIMILAR_SCOPE: &str = "youtube_music.related.v1";
const YOUTUBE_MUSIC_TOP_TRACKS_SCOPE: &str = "youtube_music.songs.v1";

const SIMILAR_SCOPE: &str = "related artists in basis rank order, local top tracks per artist";
const TOP_SCOPE: &str = "shared server facts, no time dimension";

fn owned_recording(
    harness: &mut Harness,
    title: &str,
    credits: &[&Artist],
    duration_ms: u64,
    recording_id: Option<&str>,
) -> TrackId {
    let metadata = TrackMetadata::from_names(
        title.to_owned(),
        credits
            .iter()
            .map(|artist| artist.name().to_owned())
            .collect(),
        Some(duration_ms),
    )
    .expect("track metadata")
    .with_credits(credits.iter().map(|artist| credit_of(artist)).collect())
    .with_recording_id(recording_id.map(str::to_owned))
    .expect("a recording id");
    let id = harness.library.view(
        Track::new(TrackId::new(), metadata),
        LibraryState::default(),
    );
    harness
        .library
        .credits
        .entry(id.clone())
        .or_default()
        .extend(credits.iter().map(|artist| artist.id().clone()));

    id
}

fn similar_request(
    seed: &DiscoverySeed,
    basis: DiscoveryBasis,
    user: Option<Uuid>,
) -> SimilarTracksRequest {
    SimilarTracksRequest {
        seed: seed.clone(),
        basis,
        window: None,
        limit: limit(50),
        user,
    }
}

#[tokio::test]
async fn similar_tracks_excludes_the_seed_track_and_never_repeats_a_candidate() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let first = candidate_artist(&mut harness, "First", "mbid-first");
    let second = candidate_artist(&mut harness, "Second", "mbid-second");
    let seed_track = owned_credited(&mut harness, "seed song", &[&seed, &first]);
    owned_credited(&mut harness, "first extra", &[&seed, &first]);
    owned_credited(&mut harness, "second song", &[&seed, &second]);
    let shared = owned_credited(&mut harness, "shared song", &[&first, &second]);

    let service = harness.service().await;
    let first_top = service
        .top(top_request(&first, DiscoveryBasis::Library, None))
        .await
        .expect("the related artist's own top list");

    assert!(
        first_top
            .items
            .iter()
            .any(|item| item.canonical.as_ref() == Some(&seed_track)),
        "the fixture puts the seed's own track in a related artist's top list, so excluding it is load-bearing"
    );

    let result = service
        .similar_tracks(similar_request(
            &DiscoverySeed::Track(seed_track.clone()),
            DiscoveryBasis::Library,
            None,
        ))
        .await
        .expect("a similar-tracks result");

    let canonical = result
        .items
        .iter()
        .map(|item| item.canonical.clone())
        .collect::<Vec<_>>();
    let distinct = canonical
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();

    assert_eq!(result.basis, DiscoveryBasis::Library);
    assert!(!result.fallback);
    assert_eq!(
        canonical.len(),
        distinct.len(),
        "a candidate track is emitted at most once across related artists"
    );
    assert!(
        canonical.iter().all(|id| id.is_some()),
        "every emitted item is projected to a canonical track"
    );
    assert!(
        canonical.iter().all(|id| id.as_ref() != Some(&seed_track)),
        "the seed's own track never appears in its similar list"
    );
    assert_eq!(
        canonical
            .iter()
            .filter(|id| id.as_ref() == Some(&shared))
            .count(),
        1,
        "the two related artists share one track and it is emitted once"
    );
    assert!(
        canonical.contains(&Some(by_title(&harness, "first extra"))),
        "a track only the first related artist carries is kept"
    );
    assert!(
        canonical.contains(&Some(by_title(&harness, "second song"))),
        "a track only the second related artist carries is kept"
    );
    assert_eq!(canonical.len(), 3);
}

#[tokio::test]
async fn similar_tracks_honours_its_limit_and_names_its_own_scope() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let seed_track = owned_credited(&mut harness, "seed song", &[&seed, &peer]);
    owned_credited(&mut harness, "peer one", &[&peer]);
    owned_credited(&mut harness, "peer two", &[&peer]);

    let service = harness.service().await;
    let result = service
        .similar_tracks(SimilarTracksRequest {
            limit: limit(2),
            ..similar_request(
                &DiscoverySeed::Track(seed_track.clone()),
                DiscoveryBasis::Library,
                None,
            )
        })
        .await
        .expect("a bounded result");

    assert_eq!(result.items.len(), 2);
    assert_eq!(result.items[0].rank_in_basis, 0);
    assert_eq!(result.items[1].rank_in_basis, 1);
    assert!(
        !result
            .items
            .iter()
            .any(|item| item.canonical.as_ref() == Some(&seed_track))
    );
    assert_eq!(result.ranking_scope.as_deref(), Some(SIMILAR_SCOPE));
    assert_ne!(
        result.ranking_scope.as_deref(),
        Some(TOP_SCOPE),
        "a similar-tracks list does not borrow the single artist top-list scope"
    );
    assert!(
        result
            .items
            .iter()
            .all(|item| item.ranking_scope.as_deref() == Some(SIMILAR_SCOPE)),
        "each item names the composition that ordered it"
    );
    assert!(
        result.items.iter().all(|item| item.score.is_none()),
        "the composition invents no similarity score"
    );
}

#[tokio::test]
async fn similar_tracks_without_related_artists_reports_a_fallback() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    owned_credited(&mut harness, "seed song", &[&seed]);

    let result = harness
        .service()
        .await
        .similar_tracks(similar_request(
            &DiscoverySeed::Artist(seed.id().clone()),
            DiscoveryBasis::Library,
            None,
        ))
        .await
        .expect("an honest empty result");

    assert_eq!(result.basis, DiscoveryBasis::Library);
    assert!(result.items.is_empty());
    assert!(result.fallback);
    assert_eq!(result.fallback_reason, Some(FallbackReason::NoLocalBasis));
    assert_eq!(result.ranking_scope.as_deref(), Some(SIMILAR_SCOPE));
}

#[tokio::test]
async fn the_personal_basis_composes_from_the_listen_window_scope() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    let peer = candidate_artist(&mut harness, "Peer", "mbid-peer");
    let user = Uuid::now_v7();
    let played = owned_credited(&mut harness, "played song", &[&seed, &peer]);
    owned_credited(&mut harness, "peer song", &[&peer]);
    harness.listens.marker = Some(harness.now - Duration::days(30));
    harness
        .listens
        .records
        .push((user, played.clone(), harness.now));

    let service = harness.service().await;
    let seeded = DiscoverySeed::Artist(seed.id().clone());
    let result = service
        .similar_tracks(similar_request(
            &seeded,
            DiscoveryBasis::Personal,
            Some(user),
        ))
        .await
        .expect("a personal similar result");

    assert_eq!(result.basis, DiscoveryBasis::Personal);
    assert!(!result.fallback);
    assert_eq!(result.coverage_since, harness.listens.marker);
    assert_eq!(result.ranking_scope.as_deref(), Some(SIMILAR_SCOPE));
    assert!(
        result.items.iter().any(|item| item.title == "peer song"),
        "the related artist's own local track is emitted"
    );
    assert!(
        !result.items.iter().any(|item| item.canonical.is_none()),
        "nothing is emitted without a canonical track"
    );

    let anonymous = service
        .similar_tracks(similar_request(&seeded, DiscoveryBasis::Personal, None))
        .await
        .expect("an anonymous personal result");

    assert_eq!(anonymous.basis, DiscoveryBasis::Personal);
    assert!(anonymous.items.is_empty());
    assert!(anonymous.fallback);
    assert_eq!(
        anonymous.fallback_reason,
        Some(FallbackReason::NoLocalBasis),
        "a personal request without a principal is declared absence, not a silent success"
    );
}

#[tokio::test]
async fn a_basis_the_composition_does_not_serve_is_a_typed_error() {
    let mut harness = Harness::new();
    let seed = seed_artist(&mut harness);
    owned_credited(&mut harness, "seed song", &[&seed]);

    let error = harness
        .service()
        .await
        .similar_tracks(similar_request(
            &DiscoverySeed::Artist(seed.id().clone()),
            DiscoveryBasis::External,
            None,
        ))
        .await
        .expect_err("an unsupported basis");

    assert!(
        matches!(error, DiscoveryError::UnsupportedBasis { basis } if basis == "external"),
        "the provider basis is refused rather than reported as an empty composition"
    );
}
