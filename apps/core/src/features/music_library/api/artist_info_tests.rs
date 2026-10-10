use std::{
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    Extension, Json,
    body::to_bytes,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use uuid::Uuid;

use crate::features::identity::api::native::AuthenticatedPrincipal;
use crate::features::identity::domain::{
    ApiKeyCredential, AppPasswordCredential, Principal, User, UserId, UserRole,
};
use crate::features::identity::secrets::CredentialKey;
use crate::features::identity::service::CredentialService;
use crate::features::identity::test_support::{CredentialRepository, CredentialStoreError};
use crate::features::metadata::capabilities::{
    CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
};
use crate::features::metadata::domain::{
    AttemptError, Attribution, CandidateItem, FieldId, FieldValue, FieldWrite, MatchBasis,
    MetadataEntityKind, MetricFact, StoredAttempt, StoredFact, SubjectFacts, SubjectKey,
    bytes_state,
};
use crate::features::metadata::freshness::NoJitter;
use crate::features::metadata::licences::licence_terms;
use crate::features::metadata::refresh::{DocumentError, DocumentOutcome, MetadataDocuments};
use crate::features::metadata::selection::{
    FieldOrder, LASTFM, MUSICBRAINZ, THEAUDIODB, WIKIPEDIA, YOUTUBE_MUSIC,
};
use crate::features::metadata::service::{MetadataProfile, MetadataService, RefreshOutcome};
use crate::features::metadata::storage::{CommitOutcome, MetadataRepositoryError, MetadataStore};
use crate::features::music_library::api::artist_info::{
    ArtistInfoState, info, refresh, related, request_refresh, top,
};
use crate::features::music_library::artwork::{
    ArtworkAssetDescriptor, ArtworkCache, ArtworkRole, ArtworkVariant,
};
use crate::features::music_library::catalogue::{
    LibraryService, MAX_PAGE_LIMIT, TrackListFilter, TrackOrder, TrackPage,
};
use crate::features::music_library::discovery::{
    DiscoveryBasis, DiscoverySeed, DiscoveryService, RelatedRequest, TopRequest,
};
use crate::features::music_library::domain::{
    Artist, ArtistId, EnrichmentSubject, ListenCount, ListenCoverage, ListenOutcome, ListenPage,
    ListenQuery, ListenSubmission, ListenWindow, PageLimit, ProviderId, RefreshDemand,
    SourceIdentity, SourceScope, Track, TrackId, TrackView,
};
use crate::features::music_library::storage::contracts::ArtworkStore;
use crate::features::music_library::test_support::{
    LibraryRead, ListenStore, SourceLookup, StateStore, TrackRepositoryError,
};
use crate::platform::raw_query::RawQuery;
use crate::platform::request::{RequestContext, RequestId};

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn ago(days: i64) -> DateTime<Utc> {
    Utc::now() - Duration::days(days)
}

fn lazy_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(250))
        .connect_lazy("postgres://whio:whio@127.0.0.1:1/whio")
        .expect("a lazy pool")
}

fn context() -> RequestContext {
    RequestContext::new(RequestId::generate())
}

fn principal() -> Principal {
    Principal {
        user_id: UserId::from_uuid(Uuid::now_v7()),
        role: UserRole::Member,
    }
}

fn source(provider_id: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).expect("a provider id"),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .expect("a source identity")
}

fn artist(name: &str, mbid: Option<&str>) -> Artist {
    Artist::new(
        ArtistId::new(),
        name.to_owned(),
        None,
        mbid.map(str::to_owned),
    )
    .expect("an artist")
}

struct UnavailableCredentials;

#[async_trait]
impl CredentialRepository for UnavailableCredentials {
    async fn find_user_by_username(
        &self,
        _username: &str,
    ) -> Result<Option<User>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn list_users(&self) -> Result<Vec<User>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn find_user_by_id(&self, _id: UserId) -> Result<Option<User>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn find_active_api_key_by_digest(
        &self,
        _digest: [u8; 32],
    ) -> Result<Option<ApiKeyCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn list_active_app_passwords(
        &self,
        _user_id: UserId,
    ) -> Result<Vec<AppPasswordCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }

    async fn find_active_app_password(
        &self,
        _credential_id: Uuid,
    ) -> Result<Option<AppPasswordCredential>, CredentialStoreError> {
        Err(CredentialStoreError::Unavailable)
    }
}

fn credential_service() -> Arc<CredentialService> {
    let encoded = STANDARD_NO_PAD.encode([9u8; 32]);
    let key = CredentialKey::from_base64(&encoded).expect("a credential key");

    Arc::new(CredentialService::new(
        Arc::new(UnavailableCredentials),
        key,
        crate::features::identity::LoginThrottleConfig::default(),
    ))
}

struct FakeStore {
    facts: Mutex<Vec<StoredFact>>,
    attempts: Mutex<Vec<StoredAttempt>>,
    writes: Mutex<usize>,
    reads: Mutex<usize>,
}

impl FakeStore {
    fn new(facts: Vec<StoredFact>, attempts: Vec<StoredAttempt>) -> Arc<Self> {
        Arc::new(Self {
            facts: Mutex::new(facts),
            attempts: Mutex::new(attempts),
            writes: Mutex::new(0),
            reads: Mutex::new(0),
        })
    }

    fn writes(&self) -> usize {
        *self.writes.lock().expect("the fake writes")
    }

    fn reads(&self) -> usize {
        *self.reads.lock().expect("the fake reads")
    }
}

#[async_trait]
impl MetadataStore for FakeStore {
    async fn read_subject(
        &self,
        key: &SubjectKey,
    ) -> Result<SubjectFacts, MetadataRepositoryError> {
        *self.reads.lock().expect("the fake reads") += 1;

        Ok(SubjectFacts::new(
            key.clone(),
            self.facts.lock().expect("the fake facts").clone(),
            self.attempts.lock().expect("the fake attempts").clone(),
        ))
    }

    async fn commit_field(
        &self,
        _subject: &EnrichmentSubject,
        _write: &FieldWrite,
    ) -> Result<CommitOutcome, MetadataRepositoryError> {
        *self.writes.lock().expect("the fake writes") += 1;

        Ok(CommitOutcome::Written)
    }

    async fn record_failure(
        &self,
        _subject: &EnrichmentSubject,
        _field: FieldId,
        _language: &str,
        _source_provider_id: &str,
        _error: &AttemptError,
    ) -> Result<(), MetadataRepositoryError> {
        *self.writes.lock().expect("the fake writes") += 1;

        Ok(())
    }

    async fn delete_subject_facts(
        &self,
        _kind: MetadataEntityKind,
        _ids: &[Uuid],
    ) -> Result<u64, MetadataRepositoryError> {
        Ok(0)
    }
}

struct FakeCapabilities {
    providers: Vec<String>,
}

#[async_trait]
impl CapabilitySource for FakeCapabilities {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        Ok(Snapshot::new(self.providers.clone()))
    }
}

struct FakeDocuments {
    calls: Mutex<usize>,
}

impl FakeDocuments {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(0),
        })
    }

    fn calls(&self) -> usize {
        *self.calls.lock().expect("the fake calls")
    }
}

#[async_trait]
impl MetadataDocuments for FakeDocuments {
    async fn fetch(
        &self,
        _source: &SourceIdentity,
        _profile: &MetadataProfile,
        _language: &str,
        _fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        *self.calls.lock().expect("the fake calls") += 1;

        Err(DocumentError::Unsupported)
    }
}

struct FakeLibrary {
    artist: Option<Artist>,
}

#[async_trait]
impl LibraryRead for FakeLibrary {
    async fn find_track(&self, _track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_artist(
        &self,
        _artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        Ok(self.artist.clone())
    }

    async fn page_tracks(
        &self,
        _offset: u32,
        _limit: u32,
        _order: TrackOrder,
        _filter: &TrackListFilter,
        _user: Option<Uuid>,
    ) -> Result<TrackPage, TrackRepositoryError> {
        Ok(TrackPage::new(Vec::new(), 0))
    }

    async fn track_views(
        &self,
        _track_ids: &[TrackId],
        _user: Option<Uuid>,
    ) -> Result<Vec<TrackView>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}

struct FakeSources {
    identities: Vec<SourceIdentity>,
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
        _artist_id: &ArtistId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(self.identities.clone())
    }
}

struct FakeState;

#[async_trait]
impl StateStore for FakeState {}

struct FakeListens;

#[async_trait]
impl ListenStore for FakeListens {
    async fn submit_listen(
        &self,
        _submission: &ListenSubmission,
    ) -> Result<ListenOutcome, TrackRepositoryError> {
        Ok(ListenOutcome::Recorded)
    }

    async fn listen_history(
        &self,
        _principal: Uuid,
        _query: &ListenQuery,
    ) -> Result<ListenPage, TrackRepositoryError> {
        Ok(ListenPage::new(Vec::new(), ListenCoverage::new(None)))
    }

    async fn listen_count(
        &self,
        _principal: Uuid,
        _window: ListenWindow,
        _track: Option<&TrackId>,
    ) -> Result<ListenCount, TrackRepositoryError> {
        Ok(ListenCount::new(0, ListenCoverage::new(None)))
    }
}

struct FakeArtworkStore {
    ready: Vec<ArtworkAssetDescriptor>,
    touched: Mutex<usize>,
}

impl FakeArtworkStore {
    fn new(ready: Vec<ArtworkAssetDescriptor>) -> Arc<Self> {
        Arc::new(Self {
            ready,
            touched: Mutex::new(0),
        })
    }

    fn touches(&self) -> usize {
        *self.touched.lock().expect("the fake touches")
    }
}

#[async_trait]
impl ArtworkStore for FakeArtworkStore {
    async fn find_ready(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        Ok(self
            .ready
            .iter()
            .filter(|descriptor| &descriptor.subject == subject && descriptor.role == role)
            .cloned()
            .collect())
    }

    async fn record_asset(
        &self,
        _tx: &mut sqlx::PgConnection,
        _asset: &ArtworkAssetDescriptor,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn record_assets(
        &self,
        _assets: &[ArtworkAssetDescriptor],
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn touch(&self, _id: Uuid, _now: DateTime<Utc>) -> Result<(), TrackRepositoryError> {
        *self.touched.lock().expect("the fake touches") += 1;

        Ok(())
    }

    async fn lru_descriptors(
        &self,
        _exclude: Option<Uuid>,
        _limit: u32,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn delete_asset(&self, _id: Uuid) -> Result<bool, TrackRepositoryError> {
        Ok(false)
    }
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("whio-artist-info-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&path).expect("a temporary artwork root");

        Self(path)
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("a ready asset file");
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    artist_id: Option<ArtistId>,
    library: Arc<LibraryService>,
    state: ArtistInfoState,
    discovery: Arc<DiscoveryService>,
    documents: Arc<FakeDocuments>,
    store: Arc<FakeStore>,
    snapshot: Arc<CapabilitySnapshot>,
}

impl Fixture {
    fn id(&self) -> ArtistId {
        self.artist_id.clone().expect("the fixture artist")
    }
}

struct Harness {
    artist: Option<Artist>,
    identities: Vec<SourceIdentity>,
    order: FieldOrder,
    advertised: Vec<String>,
    facts: Vec<StoredFact>,
    attempts: Vec<StoredAttempt>,
    metadata: bool,
    discovery: bool,
    artwork: Option<Arc<FakeArtworkStore>>,
    artwork_root: Option<PathBuf>,
}

impl Harness {
    fn new() -> Self {
        Self {
            artist: Some(artist("Daft Punk", Some("mbid-daft-punk"))),
            identities: Vec::new(),
            order: FieldOrder::from_entries(Vec::new()),
            advertised: Vec::new(),
            facts: Vec::new(),
            attempts: Vec::new(),
            metadata: true,
            discovery: true,
            artwork: None,
            artwork_root: None,
        }
    }

    fn artist(mut self, artist: Artist) -> Self {
        self.artist = Some(artist);

        self
    }

    fn unfound(mut self) -> Self {
        self.artist = None;

        self
    }

    fn order(mut self, order: FieldOrder) -> Self {
        self.order = order;

        self
    }

    fn advertised(mut self, providers: &[&str]) -> Self {
        self.advertised = providers
            .iter()
            .map(|provider| (*provider).to_owned())
            .collect();

        self
    }

    fn identities(mut self, identities: &[SourceIdentity]) -> Self {
        self.identities = identities.to_vec();

        self
    }

    fn facts(mut self, facts: Vec<StoredFact>) -> Self {
        self.facts = facts;

        self
    }

    fn attempts(mut self, attempts: Vec<StoredAttempt>) -> Self {
        self.attempts = attempts;

        self
    }

    fn without_metadata(mut self) -> Self {
        self.metadata = false;

        self
    }

    fn without_discovery(mut self) -> Self {
        self.discovery = false;

        self
    }

    fn artwork(mut self, store: Arc<FakeArtworkStore>, root: PathBuf) -> Self {
        self.artwork = Some(store);
        self.artwork_root = Some(root);

        self
    }

    fn build(self) -> Fixture {
        let store = FakeStore::new(self.facts, self.attempts);
        let documents = FakeDocuments::new();
        let capabilities = Arc::new(FakeCapabilities {
            providers: self.advertised,
        });
        let snapshot = Arc::new(CapabilitySnapshot::new(capabilities, CAPABILITY_TTL));
        let metadata = Arc::new(MetadataService::new(
            lazy_pool(),
            Arc::clone(&store) as Arc<dyn MetadataStore>,
            Arc::clone(&documents) as Arc<dyn MetadataDocuments>,
            Arc::clone(&snapshot),
            self.order,
            "v1".to_owned(),
            Arc::new(NoJitter),
        ));
        let artist_id = self.artist.as_ref().map(|artist| artist.id().clone());
        let read: Arc<dyn LibraryRead> = Arc::new(FakeLibrary {
            artist: self.artist,
        });
        let sources: Arc<dyn SourceLookup> = Arc::new(FakeSources {
            identities: self.identities,
        });
        let library_state: Arc<dyn StateStore> = Arc::new(FakeState);
        let mut library = LibraryService::new(
            Arc::clone(&read),
            Arc::clone(&sources),
            Arc::clone(&library_state),
        );
        if self.metadata {
            library = library.with_metadata(Arc::clone(&metadata));
        }
        if let Some(artwork) = self.artwork.as_ref() {
            library = library.with_artwork_cache(Arc::new(ArtworkCache::new(
                Arc::clone(artwork) as Arc<dyn ArtworkStore>,
                self.artwork_root.clone(),
                1024 * 1024,
            )));
        }
        let library = Arc::new(library);
        let mut discovery = DiscoveryService::new(
            read,
            sources,
            library_state,
            Arc::new(FakeListens) as Arc<dyn ListenStore>,
        );
        if self.metadata {
            discovery = discovery.with_metadata(metadata);
        }
        let discovery = Arc::new(discovery);
        let state = ArtistInfoState::with_defaults(
            Arc::clone(&library),
            credential_service(),
            MetadataProfile::artist_display(),
            "en".to_owned(),
            self.discovery.then(|| Arc::clone(&discovery)),
        );

        Fixture {
            artist_id,
            library,
            state,
            discovery,
            documents,
            store,
            snapshot,
        }
    }
}

fn fact(
    field: FieldId,
    provider_id: &str,
    value: FieldValue,
    fetched_at: DateTime<Utc>,
) -> StoredFact {
    let licence = licence_terms(provider_id, 1).expect("the registry terms");

    StoredFact {
        field,
        source: crate::features::metadata::domain::FactSource {
            provider_id: provider_id.to_owned(),
            external_id: format!("{provider_id}-{field:?}"),
            url: None,
            match_basis: MatchBasis::Identifier,
        },
        value,
        bytes: bytes_state(field, &licence),
        licence,
        attribution: None,
        source_url: None,
        operation: "artist.biography.v1".to_owned(),
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

fn with_metric(fact: StoredFact, metric: MetricFact) -> StoredFact {
    StoredFact {
        metric: Some(metric),
        ..fact
    }
}

fn with_attribution(fact: StoredFact, text: &str) -> StoredFact {
    StoredFact {
        attribution: Some(Attribution {
            text: text.to_owned(),
            url: Some("https://example.test/credit".to_owned()),
        }),
        ..fact
    }
}

fn with_candidates(fact: StoredFact, candidates: Vec<CandidateItem>) -> StoredFact {
    StoredFact {
        value: FieldValue::Candidates(candidates),
        rank: Some(1),
        score: Some(0.75),
        ..fact
    }
}

fn order_for(entries: &[(FieldId, &[&str])]) -> FieldOrder {
    FieldOrder::from_entries(entries.iter().map(|(field, sources)| {
        (
            *field,
            sources.iter().map(|source| (*source).to_owned()).collect(),
        )
    }))
}

fn failed_attempt(field: FieldId, provider_id: &str, attempted_at: DateTime<Utc>) -> StoredAttempt {
    StoredAttempt {
        field,
        source_provider_id: provider_id.to_owned(),
        fetched_at: None,
        attempted_at,
        error: Some(AttemptError {
            class: "provider_unavailable".to_owned(),
            message: "the metadata provider is unavailable".to_owned(),
        }),
    }
}

async fn split<T: Serialize, E: IntoResponse>(
    result: Result<Json<T>, E>,
) -> Result<Value, (StatusCode, Value)> {
    match result {
        Ok(Json(value)) => Ok(serde_json::to_value(value).expect("the body serialises")),
        Err(error) => {
            let response = error.into_response();
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 64 * 1024)
                .await
                .expect("the error body");
            let value = serde_json::from_slice(&bytes).expect("a json error body");

            Err((status, value))
        }
    }
}

async fn send<T, E, F, Fut>(
    fixture: &Fixture,
    artist_id: &ArtistId,
    query: &str,
    handler: F,
) -> Result<Value, (StatusCode, Value)>
where
    T: Serialize,
    E: IntoResponse,
    F: FnOnce(
        State<ArtistInfoState>,
        Extension<RequestContext>,
        AuthenticatedPrincipal,
        Path<String>,
        RawQuery,
    ) -> Fut,
    Fut: Future<Output = Result<Json<T>, E>>,
{
    fixture
        .snapshot
        .current(Utc::now())
        .await
        .expect("the fixture capability source answers");

    split(
        handler(
            State(fixture.state.clone()),
            Extension(context()),
            AuthenticatedPrincipal(principal()),
            Path(artist_id.to_string()),
            RawQuery::parse(query),
        )
        .await,
    )
    .await
}

fn field<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["fields"]
        .as_array()
        .expect("the field list")
        .iter()
        .find(|entry| entry["field"] == name)
        .unwrap_or_else(|| panic!("the `{name}` field"))
}

fn populated() -> Fixture {
    let order = order_for(&[
        (FieldId::BiographyContent, &[WIKIPEDIA]),
        (FieldId::BiographySummary, &[WIKIPEDIA]),
        (FieldId::ImagePortrait, &[THEAUDIODB]),
        (FieldId::Similar, &[YOUTUBE_MUSIC]),
        (FieldId::MetricListeners, &[LASTFM]),
        (FieldId::Tag, &[MUSICBRAINZ]),
    ]);
    let facts = vec![
        with_attribution(
            fact(
                FieldId::BiographyContent,
                WIKIPEDIA,
                FieldValue::Text("Daft Punk are a French electronic duo.".to_owned()),
                now(),
            ),
            "Wikipedia contributors",
        ),
        fact(
            FieldId::BiographySummary,
            WIKIPEDIA,
            FieldValue::Text("A French electronic duo.".to_owned()),
            ago(30),
        ),
        fact(
            FieldId::ImagePortrait,
            THEAUDIODB,
            FieldValue::Reference("https://example.test/portrait.jpg".to_owned()),
            now(),
        ),
        with_candidates(
            fact(
                FieldId::Similar,
                YOUTUBE_MUSIC,
                FieldValue::Candidates(Vec::new()),
                now(),
            ),
            vec![CandidateItem {
                name: "Justice".to_owned(),
                external_id: Some("channel-justice".to_owned()),
                url: None,
                rank: Some(1),
                score: Some(0.9),
                artists: Vec::new(),
                release_name: None,
                duration_ms: None,
                isrc: None,
                recording_id: None,
                ranking_scope: None,
            }],
        ),
        with_metric(
            fact(
                FieldId::MetricListeners,
                LASTFM,
                FieldValue::Count(12_345),
                now(),
            ),
            MetricFact {
                unit: "listeners".to_owned(),
                population: Some("artist".to_owned()),
                period: Some("all-time".to_owned()),
                as_of: Some(ago(1)),
            },
        ),
    ];
    let attempts = vec![failed_attempt(FieldId::Tag, MUSICBRAINZ, now())];

    Harness::new()
        .order(order)
        .advertised(&[MUSICBRAINZ])
        .facts(facts)
        .attempts(attempts)
        .build()
}

#[tokio::test]
async fn info_reports_the_canonical_artist_and_an_unrequested_absence_without_facts() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let body = send(&fixture, &id, "", info)
        .await
        .expect("a served artist");

    assert_eq!(body["id"], id.to_string());
    assert_eq!(body["name"], "Daft Punk");
    assert_eq!(body["music_brainz_id"], "mbid-daft-punk");
    assert!(body.get("sort_name").is_none());
    assert_eq!(body["cover_art"], format!("ar-{id}"));
    assert_eq!(body["images"], json!([]));
    assert_eq!(body["disagreements"], json!([]));
    assert_eq!(body["contested"], false);

    let fields = body["fields"].as_array().expect("the field list");
    assert_eq!(fields.len(), 16);
    for entry in fields {
        assert_eq!(entry["presence"], "absent", "{entry}");
        assert_eq!(entry["absence"], "unrequested", "{entry}");
        assert_eq!(entry["freshness"], "unrequested", "{entry}");
        assert!(entry.get("value").is_none(), "{entry}");
        assert!(entry.get("source").is_none(), "{entry}");
        assert!(entry.get("bytes").is_none(), "{entry}");
    }

    assert_eq!(body["refresh"]["state"], "unavailable");
    assert!(
        body["refresh"]["revision"]
            .as_str()
            .is_some_and(|revision| revision.len() == 64)
    );
}

#[tokio::test]
async fn info_projects_cached_facts_with_provenance_freshness_and_metrics() {
    let fixture = populated();
    let id = fixture.id();
    let body = send(&fixture, &id, "", info)
        .await
        .expect("a served artist");

    let biography = field(&body, "biography.content");
    assert_eq!(biography["presence"], "present");
    assert_eq!(biography["freshness"], "fresh");
    assert_eq!(biography["value"], "Daft Punk are a French electronic duo.");
    assert_eq!(biography["source"]["provider_id"], WIKIPEDIA);
    assert_eq!(biography["source"]["operation"], "artist.biography.v1");
    assert_eq!(
        biography["source"]["external_id"],
        "wikipedia-BiographyContent"
    );
    assert_eq!(biography["source"]["match_basis"], "identifier");
    assert_eq!(biography["licence"]["id"], WIKIPEDIA);
    assert_eq!(biography["licence"]["attribution_required"], true);
    assert_eq!(biography["attribution"]["text"], "Wikipedia contributors");
    assert_eq!(
        biography["attribution"]["url"],
        "https://example.test/credit"
    );
    assert!(
        biography["retrieved_at"]
            .as_str()
            .is_some_and(|value| value.ends_with("+00:00"))
    );
    assert!(biography.get("absence").is_none());

    let stale = field(&body, "biography.summary");
    assert_eq!(stale["presence"], "present");
    assert_eq!(stale["freshness"], "stale");
    assert_eq!(stale["value"], "A French electronic duo.");

    let portrait = field(&body, "image.portrait");
    assert_eq!(portrait["presence"], "present");
    assert_eq!(portrait["bytes"], "reference_only");
    assert_eq!(portrait["value"], "https://example.test/portrait.jpg");
    assert_eq!(portrait["licence"]["redistribution_allowed"], false);

    let listeners = field(&body, "metric.listeners");
    assert_eq!(listeners["presence"], "present");
    assert_eq!(listeners["value"], 12_345);
    assert_eq!(listeners["unit"], "listeners");
    assert_eq!(listeners["population"], "artist");
    assert_eq!(listeners["period"], "all-time");
    assert!(listeners["as_of"].as_str().is_some());

    let similar = field(&body, "similar");
    assert_eq!(similar["presence"], "present");
    assert_eq!(similar["rank"], 1);
    assert_eq!(similar["score"], 0.75);
    assert_eq!(similar["value"][0]["name"], "Justice");
    assert_eq!(similar["value"][0]["external_id"], "channel-justice");
    assert_eq!(similar["value"][0]["rank"], 1);
    assert_eq!(similar["value"][0]["score"], 0.9);

    assert_eq!(body["contested"], false);
    assert_eq!(body["disagreements"], json!([]));
    assert_eq!(fixture.documents.calls(), 0);
}

#[tokio::test]
async fn a_failed_field_reports_the_failure_without_an_absence_reason() {
    let fixture = populated();
    let id = fixture.id();
    let body = send(&fixture, &id, "", info)
        .await
        .expect("a served artist");

    let tag = field(&body, "tag");
    assert_eq!(tag["presence"], "absent");
    assert_eq!(tag["freshness"], "failed");
    assert!(tag.get("absence").is_none());
    assert!(tag.get("value").is_none());
    assert!(tag.get("source").is_none());
}

#[tokio::test]
async fn an_eligible_source_without_a_job_store_is_a_typed_503() {
    let order = order_for(&[(FieldId::BiographyContent, &[WIKIPEDIA])]);
    let fixture = Harness::new()
        .order(order)
        .advertised(&[WIKIPEDIA])
        .identities(&[source(WIKIPEDIA, "wiki-1")])
        .build();
    let id = fixture.id();
    let (status, body) = send(&fixture, &id, "", info)
        .await
        .expect_err("an unavailable job store");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "metadata_unavailable");
    assert_eq!(fixture.store.writes(), 0);
}

#[tokio::test]
async fn a_capability_absent_field_keeps_the_reason_the_metadata_owner_gave_it() {
    let order = order_for(&[(FieldId::BiographyContent, &[WIKIPEDIA])]);
    let fixture = Harness::new().order(order).build();
    let id = fixture.id();
    let body = send(&fixture, &id, "", info)
        .await
        .expect("a served artist");

    assert_eq!(
        field(&body, "biography.content")["absence"],
        "capability_absent"
    );
    assert_eq!(
        field(&body, "biography.content")["freshness"],
        "unrequested"
    );
    assert_eq!(field(&body, "tag")["absence"], "unrequested");
    assert_eq!(body["refresh"]["state"], "unavailable");
}

#[tokio::test]
async fn a_ready_portrait_is_described_while_a_reference_only_value_is_not_served() {
    let root = TempRoot::new();
    let artist = artist("Daft Punk", Some("mbid-daft-punk"));
    let artist_id = artist.id().clone();
    root.write("portrait.jpg", b"jpeg");
    let descriptor = ArtworkAssetDescriptor {
        id: Uuid::now_v7(),
        subject: artist_id,
        role: ArtworkRole::Portrait,
        source_provider_id: THEAUDIODB.to_owned(),
        source_external_id: "theaudiodb-BiographyContent".to_owned(),
        revision: 3,
        variant: ArtworkVariant::Original,
        content_digest: "abc123".to_owned(),
        content_type: "image/jpeg".to_owned(),
        width: 600,
        height: 600,
        byte_size: 4,
        file_name: "portrait.jpg".to_owned(),
    };
    let store = FakeArtworkStore::new(vec![descriptor]);
    let order = order_for(&[(FieldId::ImagePortrait, &[THEAUDIODB])]);
    let facts = vec![fact(
        FieldId::ImagePortrait,
        THEAUDIODB,
        FieldValue::Reference("https://example.test/portrait.jpg".to_owned()),
        now(),
    )];
    let fixture = Harness::new()
        .artist(artist)
        .order(order)
        .facts(facts)
        .artwork(Arc::clone(&store), root.0.clone())
        .build();
    let id = fixture.id();
    let body = send(&fixture, &id, "", info)
        .await
        .expect("a served artist");

    assert_eq!(field(&body, "image.portrait")["bytes"], "reference_only");
    assert_eq!(body["images"][0]["role"], "portrait");
    assert_eq!(body["images"][0]["size"], Value::Null);
    assert_eq!(body["images"][0]["width"], 600);
    assert_eq!(body["images"][0]["height"], 600);
    assert_eq!(body["images"][0]["content_type"], "image/jpeg");
    assert_eq!(body["images"][0]["revision"], 3);
    assert_eq!(body["images"][0]["etag"], "\"abc123\"");
    assert_eq!(body["images"].as_array().expect("the images").len(), 1);
    assert_eq!(store.touches(), 1);
}

#[tokio::test]
async fn no_read_calls_a_provider_document() {
    let fixture = populated();
    let id = fixture.id();

    send(&fixture, &id, "", info)
        .await
        .expect("a served artist");
    send(&fixture, &id, "", refresh)
        .await
        .expect("a served status");
    send(&fixture, &id, "", request_refresh)
        .await
        .expect("a served outcome");
    send(&fixture, &id, "", related)
        .await
        .expect("a served basis");
    send(&fixture, &id, "", top).await.expect("a served basis");

    assert_eq!(fixture.documents.calls(), 0);
}

#[tokio::test]
async fn a_status_read_reports_the_observed_phase_and_enrols_no_work() {
    let fixture = populated();
    let id = fixture.id();
    let body = send(&fixture, &id, "", refresh)
        .await
        .expect("a served status");

    assert_eq!(body["state"], "unavailable");
    assert!(body["revision"].as_str().is_some());
    assert!(body.get("outcome").is_none());
    assert_eq!(fixture.store.writes(), 0);
    assert_eq!(fixture.documents.calls(), 0);
}

#[tokio::test]
async fn a_forced_refresh_reports_the_metadata_outcome_without_a_source() {
    let fixture = populated();
    let id = fixture.id();
    let body = send(&fixture, &id, "", request_refresh)
        .await
        .expect("a served outcome");

    assert_eq!(body["outcome"], "unavailable");
    assert_eq!(fixture.store.writes(), 0);
    assert_eq!(fixture.documents.calls(), 0);
}

#[tokio::test]
async fn the_composition_maps_each_demand_onto_the_metadata_operation() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let profile = MetadataProfile::artist_display();

    let read_only = fixture
        .library
        .artist_information(&id, None, &profile, "en", RefreshDemand::ReadOnly)
        .await
        .expect("a readable artist")
        .expect("a known artist");
    assert_eq!(read_only.refresh.outcome, None);
    assert!(read_only.portraits.is_empty());
    assert_eq!(read_only.cover_art, format!("ar-{id}"));
    assert_eq!(read_only.metadata.fields.len(), 16);

    for refresh in [RefreshDemand::RequestIfDue, RefreshDemand::Force] {
        let demanded = fixture
            .library
            .artist_information(&id, None, &profile, "en", refresh)
            .await
            .expect("a readable artist")
            .expect("a known artist");

        assert_eq!(demanded.refresh.outcome, Some(RefreshOutcome::Unavailable));
    }

    assert_eq!(fixture.documents.calls(), 0);
    assert_eq!(fixture.store.writes(), 0);
}

#[tokio::test]
async fn the_composition_is_absent_for_an_unknown_artist() {
    let fixture = Harness::new().unfound().build();
    let absent = fixture
        .library
        .artist_information(
            &ArtistId::new(),
            None,
            &MetadataProfile::artist_display(),
            "en",
            RefreshDemand::RequestIfDue,
        )
        .await
        .expect("a readable artist");

    assert!(absent.is_none());
}

#[tokio::test]
async fn info_is_a_typed_503_when_the_metadata_feature_is_disabled() {
    let fixture = Harness::new().without_metadata().build();
    let id = fixture.id();
    let (status, body) = send(&fixture, &id, "", info)
        .await
        .expect_err("a disabled metadata feature");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "metadata_disabled");
}

#[tokio::test]
async fn info_rejects_an_unknown_artist_and_a_malformed_identifier() {
    let fixture = Harness::new().unfound().build();
    let (status, body) = send(&fixture, &ArtistId::new(), "", info)
        .await
        .expect_err("an unknown artist");

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "not_found");

    let fixture = Harness::new().build();
    let (status, body) = split::<_, _>(
        info(
            State(fixture.state.clone()),
            Extension(context()),
            AuthenticatedPrincipal(principal()),
            Path("not-a-uuid".to_owned()),
            RawQuery::parse(""),
        )
        .await,
    )
    .await
    .expect_err("a malformed identifier");

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn info_rejects_an_unknown_parameter_a_profile_and_a_language() {
    let fixture = Harness::new().build();
    let id = fixture.id();

    for query in [
        "offset=1",
        "profile=artist-other.v1",
        "language=123",
        "language=",
        "limit=10",
    ] {
        let (status, body) = send(&fixture, &id, query, info)
            .await
            .expect_err("a rejected request");

        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["code"], "invalid_request", "{query}");
    }
}

#[tokio::test]
async fn info_accepts_the_default_profile_and_a_language_subtag() {
    let fixture = Harness::new().build();
    let id = fixture.id();

    for query in [
        "",
        "profile=artist-display.v1",
        "language=en",
        "language=en-GB",
    ] {
        let body = send(&fixture, &id, query, info)
            .await
            .unwrap_or_else(|_| panic!("`{query}` is accepted"));

        assert_eq!(body["id"], id.to_string(), "{query}");
    }
}

async fn assert_top_projects_the_shared_result(
    fixture: &Fixture,
    body: &Value,
    basis: DiscoveryBasis,
) {
    let result = fixture
        .discovery
        .top(TopRequest {
            seed: DiscoverySeed::Artist(fixture.id()),
            basis,
            window: None,
            limit: PageLimit::new(50).expect("the page limit"),
            user: None,
        })
        .await
        .expect("the shared composition");

    assert_eq!(body["basis"], result.basis.as_str());
    assert_eq!(body["ranking_scope"], json!(result.ranking_scope));
    assert_eq!(body["fallback"], result.fallback);
    assert_eq!(
        body["fallback_reason"],
        result
            .fallback_reason
            .map_or(Value::Null, |reason| json!(reason.as_str()))
    );
}

async fn assert_related_projects_the_shared_result(fixture: &Fixture, body: &Value) {
    let result = fixture
        .discovery
        .related(RelatedRequest {
            seed: DiscoverySeed::Artist(fixture.id()),
            basis: DiscoveryBasis::Library,
            window: None,
            limit: PageLimit::new(50).expect("the page limit"),
            user: None,
        })
        .await
        .expect("the shared composition");

    assert_eq!(body["basis"], result.basis.as_str());
    assert_eq!(body["ranking_scope"], json!(result.ranking_scope));
    assert_eq!(body["fallback"], result.fallback);
    assert_eq!(
        body["fallback_reason"],
        result
            .fallback_reason
            .map_or(Value::Null, |reason| json!(reason.as_str()))
    );
}

#[tokio::test]
async fn related_projects_the_shared_library_composition() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let body = send(&fixture, &id, "", related)
        .await
        .expect("a served basis");

    assert_eq!(body["basis"], "library");
    assert_eq!(
        body["ranking_scope"],
        "shared track artist credits, no time dimension"
    );
    assert_eq!(body["fallback"], true);
    assert_eq!(body["fallback_reason"], "no_local_basis");
    assert_eq!(body["items"], json!([]));
    assert_eq!(
        fixture.store.reads(),
        0,
        "the library basis never consults the provider facts"
    );
    assert_eq!(fixture.documents.calls(), 0);
    assert_related_projects_the_shared_result(&fixture, &body).await;
}

#[tokio::test]
async fn top_projects_the_shared_library_composition() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let body = send(&fixture, &id, "", top).await.expect("a served basis");

    assert_eq!(body["basis"], "library");
    assert_eq!(body["ranking_scope"], "owned tracks in title order");
    assert_eq!(body["fallback"], true);
    assert_eq!(body["fallback_reason"], "no_local_basis");
    assert_eq!(body["items"], json!([]));
    assert_eq!(fixture.store.reads(), 0);
    assert_top_projects_the_shared_result(&fixture, &body, DiscoveryBasis::Library).await;
}

#[tokio::test]
async fn top_serves_the_personal_basis_with_the_all_time_ranking_scope() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let body = send(&fixture, &id, "basis=personal", top)
        .await
        .expect("a served basis");

    assert_eq!(body["basis"], "personal");
    assert_eq!(body["ranking_scope"], "all-time play counter");
    assert_eq!(body["fallback"], true);
    assert_eq!(body["fallback_reason"], "no_local_basis");
    assert_eq!(body["items"], json!([]));
}

#[tokio::test]
async fn top_declares_the_local_fallback_for_the_external_basis_without_provider_facts() {
    let fixture = Harness::new().build();
    let id = fixture.id();
    let body = send(&fixture, &id, "basis=external", top)
        .await
        .expect("a declared fallback");

    assert_eq!(body["basis"], "library");
    assert_eq!(
        body["ranking_scope"],
        "shared server facts, no time dimension"
    );
    assert_eq!(body["fallback"], true);
    assert_eq!(body["fallback_reason"], "no_provider_facts");
    assert_eq!(body["items"], json!([]));
    assert_eq!(
        fixture.store.reads(),
        1,
        "the external basis reads the provider facts exactly once"
    );
    assert_eq!(
        fixture.documents.calls(),
        0,
        "no provider document is fetched by a discovery read"
    );
}

#[tokio::test]
async fn a_provider_top_candidate_without_a_verified_match_carries_no_track_id() {
    let order = order_for(&[(FieldId::TopTracks, &[YOUTUBE_MUSIC])]);
    let facts = vec![with_candidates(
        fact(
            FieldId::TopTracks,
            YOUTUBE_MUSIC,
            FieldValue::Candidates(Vec::new()),
            now(),
        ),
        vec![CandidateItem {
            name: "One More Time".to_owned(),
            external_id: Some("video-one-more-time".to_owned()),
            url: None,
            rank: Some(1),
            score: Some(0.8),
            artists: vec!["Daft Punk".to_owned()],
            release_name: Some("Discovery".to_owned()),
            duration_ms: Some(320_000),
            isrc: None,
            recording_id: None,
            ranking_scope: Some("ytm.top_tracks.v1".to_owned()),
        }],
    )];
    let fixture = Harness::new()
        .order(order)
        .advertised(&[YOUTUBE_MUSIC])
        .identities(&[source(YOUTUBE_MUSIC, "channel-daft-punk")])
        .facts(facts)
        .build();
    let id = fixture.id();
    let body = send(&fixture, &id, "basis=external", top)
        .await
        .expect("the provider basis");

    assert_eq!(body["basis"], "external");
    assert_eq!(body["ranking_scope"], "ytm.top_tracks.v1");
    assert_eq!(body["fallback"], false);
    let items = body["items"].as_array().expect("the item list");
    assert_eq!(items.len(), 1);
    assert!(items[0].get("track_id").is_none(), "{}", items[0]);
    assert_eq!(items[0]["title"], "One More Time");
    assert_eq!(items[0]["provider"], YOUTUBE_MUSIC);
    assert_eq!(items[0]["external_id"], "video-one-more-time");
    assert_eq!(items[0]["rank_in_basis"], 0);
    assert_eq!(items[0]["score"], 0.8);
    assert_eq!(fixture.documents.calls(), 0);
}

#[tokio::test]
async fn discovery_reads_report_a_typed_503_without_a_configured_discovery_service() {
    let fixture = Harness::new().without_discovery().build();
    let id = fixture.id();

    let (status, body) = send(&fixture, &id, "", related)
        .await
        .expect_err("an unconfigured discovery feature");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "discovery_unavailable");

    let (status, body) = send(&fixture, &id, "", top)
        .await
        .expect_err("an unconfigured discovery feature");

    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["code"], "discovery_unavailable");
}

#[tokio::test]
async fn top_serves_every_basis_this_instance_offers_and_refuses_only_the_server_one() {
    let fixture = Harness::new().build();
    let id = fixture.id();

    for query in [
        "",
        "basis=library",
        "basis=personal",
        "basis=external",
        "limit=10",
    ] {
        let body = send(&fixture, &id, query, top)
            .await
            .unwrap_or_else(|_| panic!("`{query}` is served"));

        assert!(
            matches!(
                body["basis"].as_str(),
                Some("library" | "personal" | "external")
            ),
            "{query}: {}",
            body["basis"]
        );
    }

    let (status, body) = send(&fixture, &id, "basis=server", top)
        .await
        .expect_err("an unoffered basis");

    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(body["code"], "unsupported_basis");
}

#[tokio::test]
async fn top_rejects_a_basis_this_instance_does_not_know() {
    let fixture = Harness::new().build();
    let id = fixture.id();

    for query in [
        "basis=local",
        "basis=",
        "basis=personal,external",
        "basis=banana",
    ] {
        let (status, body) = send(&fixture, &id, query, top)
            .await
            .expect_err("a rejected request");

        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["code"], "invalid_request", "{query}");
    }
}

#[tokio::test]
async fn discovery_reads_reject_an_unknown_parameter_and_an_over_limit_window() {
    let fixture = Harness::new().build();
    let id = fixture.id();

    for query in [
        "offset=1",
        "basis=personal",
        &format!("limit={}", MAX_PAGE_LIMIT + 1),
        "limit=0",
    ] {
        let (status, body) = send(&fixture, &id, query, related)
            .await
            .expect_err("a rejected request");

        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
        assert_eq!(body["code"], "invalid_request", "{query}");
    }

    let (status, body) = send(&fixture, &id, "offset=1", top)
        .await
        .expect_err("an unknown parameter");

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");

    let (status, body) = send(&fixture, &id, "basis=personal", request_refresh)
        .await
        .expect_err("an unknown parameter");

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "invalid_request");
}

#[tokio::test]
async fn discovery_reads_report_an_unknown_artist() {
    let fixture = Harness::new().unfound().build();
    let id = ArtistId::new();

    for (status, body) in [
        send(&fixture, &id, "", related)
            .await
            .expect_err("an unknown artist"),
        send(&fixture, &id, "", top)
            .await
            .expect_err("an unknown artist"),
    ] {
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
    }
}
