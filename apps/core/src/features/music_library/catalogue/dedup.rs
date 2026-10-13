use std::{collections::BTreeSet, sync::Arc};

use crate::{
    features::music_library::{
        domain::{MatchDecision, MatchRecord, ProviderKind, RecordingIndex, TrackHit},
        storage::contracts::{LibraryRead, SourceLookup, StateStore},
    },
    platform::request::RequestContext,
};

use super::CANDIDATE_LIMIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupMode {
    Off,
    Shadow,
    Suppress,
}

pub(crate) const DEFAULT_DEDUP_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

pub(crate) struct Ports<'a> {
    pub(crate) library: &'a Arc<dyn LibraryRead>,
    pub(crate) sources: &'a Arc<dyn SourceLookup>,
    pub(crate) state: &'a Arc<dyn StateStore>,
}

pub(crate) async fn apply(
    ports: Ports<'_>,
    mut known: Vec<TrackHit>,
    mut fresh: Vec<TrackHit>,
    user: Option<uuid::Uuid>,
    context: &RequestContext,
    mode: DedupMode,
    budget: std::time::Duration,
) -> (Vec<TrackHit>, Vec<TrackHit>) {
    if mode == DedupMode::Off {
        return (known, fresh);
    }
    let started = std::time::Instant::now();
    let hits = known.iter().chain(&fresh).cloned().collect::<Vec<_>>();
    if mode == DedupMode::Shadow {
        let _ = tokio::time::timeout(budget, shadow(ports.library, ports.sources, &hits, context))
            .await;
        return (known, fresh);
    }
    match tokio::time::timeout(
        budget,
        replacements(ports.library, ports.sources, ports.state, &hits, user),
    )
    .await
    {
        Ok(Ok(replacements)) => {
            tracing::info!(request_id = %context.request_id().as_str(), replaced = replacements.len(), elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, "dedup suppression");
            for hit in known.iter_mut().chain(&mut fresh) {
                if let Some(local) = replacements.get(hit.track().id()) {
                    *hit = local.clone();
                }
            }
        }
        Ok(Err(error)) => tracing::warn!(%error, "dedup skipped; keeping remote results"),
        Err(_) => tracing::warn!("dedup budget exceeded; keeping remote results"),
    }
    (known, fresh)
}

async fn replacements(
    library: &Arc<dyn LibraryRead>,
    sources: &Arc<dyn SourceLookup>,
    state: &Arc<dyn StateStore>,
    hits: &[TrackHit],
    user: Option<uuid::Uuid>,
) -> Result<
    std::collections::HashMap<crate::features::music_library::domain::TrackId, TrackHit>,
    crate::features::music_library::storage::contracts::TrackRepositoryError,
> {
    use crate::features::music_library::domain::{MatchStatus, normalize_for_matching};
    let remotes =
        hits.iter()
            .filter(|hit| {
                !hit.sources()
                    .iter()
                    .any(|source| source.provider_id().kind() == Some(ProviderKind::Local))
                    && hit.sources().iter().any(|source| {
                        source.provider_id().kind() == Some(ProviderKind::YoutubeMusic)
                    })
            })
            .collect::<Vec<_>>();
    let mut out = std::collections::HashMap::new();
    if remotes.is_empty() {
        return Ok(out);
    }
    let keys = remotes
        .iter()
        .map(|hit| hit.track().title().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let candidates = library.dedup_candidates(&keys, CANDIDATE_LIMIT + 1).await?;
    if candidates.len() > CANDIDATE_LIMIT as usize {
        tracing::warn!("dedup candidate limit exceeded; keeping remote results");
        return Ok(out);
    }
    let index = RecordingIndex::new(candidates.iter().map(local_record).collect());
    let mut checked = std::collections::HashMap::new();
    for hit in remotes {
        let remote = record(hit.track().id().to_string(), hit);
        let decision = index.match_record(&remote, chrono::Utc::now().timestamp());
        if !matches!(
            decision.status(),
            MatchStatus::Verified | MatchStatus::MetadataCandidate
        ) {
            continue;
        }
        let Some(local) = candidates
            .iter()
            .find(|local| Some(local.track().id().to_string().as_str()) == decision.target())
        else {
            continue;
        };
        if normalize_for_matching(hit.track().title())
            != normalize_for_matching(local.track().title())
        {
            continue;
        }
        if !checked.contains_key(local.track().id()) {
            let ready = if local_available(sources, local).await? {
                let mut local = local.clone();
                if let Some(user) = user {
                    let library_state = state.find_track_state(local.track().id(), user).await?;
                    local = local.with_state(library_state);
                }
                Some(local)
            } else {
                None
            };
            checked.insert(local.track().id().clone(), ready);
        }
        if let Some(Some(local)) = checked.get(local.track().id()) {
            out.insert(hit.track().id().clone(), local.clone());
        }
    }
    Ok(out)
}

async fn local_available(
    sources: &Arc<dyn SourceLookup>,
    hit: &TrackHit,
) -> Result<bool, crate::features::music_library::storage::contracts::TrackRepositoryError> {
    for source in hit.sources() {
        if source.provider_id().kind() != Some(ProviderKind::Local) {
            continue;
        }
        let crate::features::music_library::domain::SourceScope::Named(root) =
            source.source_scope()
        else {
            continue;
        };
        let Some(facts) = sources.find_source_file_facts(source).await? else {
            continue;
        };
        if facts.missing_at.is_some() {
            continue;
        }
        let Some(relative) = facts.path.as_deref() else {
            continue;
        };
        let Some(path) = crate::platform::source_path::local_path(root, relative) else {
            continue;
        };
        if tokio::fs::metadata(path)
            .await
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) async fn shadow(
    library: &Arc<dyn LibraryRead>,
    _sources: &Arc<dyn SourceLookup>,
    fresh: &[TrackHit],
    context: &RequestContext,
) -> Vec<(String, MatchDecision)> {
    let remotes = fresh.iter().filter_map(remote_record).collect::<Vec<_>>();
    if remotes.is_empty() {
        return Vec::new();
    }
    let keys = remotes
        .iter()
        .map(MatchRecord::retrieval_key)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let candidates = match library.dedup_candidates(&keys, CANDIDATE_LIMIT).await {
        Ok(candidates) => candidates,
        Err(error) => {
            tracing::warn!(%error, "dedup shadow could not load candidates");
            return Vec::new();
        }
    };
    if candidates.is_empty() {
        return Vec::new();
    }

    let index = RecordingIndex::new(candidates.iter().map(local_record).collect());
    let now = chrono::Utc::now().timestamp();
    let mut decisions = Vec::with_capacity(remotes.len());
    for remote in remotes {
        let decision = index.match_record(&remote, now);
        tracing::info!(
            request_id = %context.request_id().as_str(),
            video_id = %remote.id(),
            status = decision.status().as_str(),
            reason = decision.reason(),
            target = decision.target().unwrap_or_default(),
            examined = decision.examined(),
            "dedup shadow decision"
        );
        decisions.push((remote.id().to_owned(), decision));
    }

    decisions
}

fn remote_record(hit: &TrackHit) -> Option<MatchRecord> {
    let source = hit.sources().first()?;
    if source.provider_id().kind() == Some(ProviderKind::Local) {
        return None;
    }

    Some(record(source.external_id(), hit))
}

fn local_record(hit: &TrackHit) -> MatchRecord {
    record(hit.track().id().to_string(), hit)
}

fn record(id: impl Into<String>, hit: &TrackHit) -> MatchRecord {
    MatchRecord::new(
        id,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "postgres-integration")]
    use crate::features::music_library::{
        domain::{Track, TrackId, TrackMetadata},
        storage::postgres::PostgresTrackRepository,
    };
    use crate::{
        features::music_library::domain::{ProviderId, SourceIdentity, SourceScope},
        platform::request::RequestId,
    };

    fn provider_source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    fn local_source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            "path-a".to_owned(),
        )
        .unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["A".to_owned()], Some(240_000)).unwrap()
    }

    #[cfg(feature = "postgres-integration")]
    fn hit(track: Track, source: SourceIdentity) -> TrackHit {
        TrackHit::new(track, None, vec![source])
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-1".to_owned()).unwrap())
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn shadow_matches_a_provider_hit_against_a_local_track(pool: sqlx::PgPool) {
        let store = Arc::new(PostgresTrackRepository::new(pool));
        let local =
            crate::features::music_library::storage::contracts::CatalogueWrite::get_or_create(
                store.as_ref(),
                local_source(),
                metadata("Song"),
            )
            .await
            .unwrap();
        let remote = Track::new(TrackId::new(), metadata("Song"));

        let decisions = shadow(
            &(Arc::clone(&store) as Arc<dyn LibraryRead>),
            &(Arc::clone(&store) as Arc<dyn SourceLookup>),
            &[hit(remote, provider_source())],
            &context(),
        )
        .await;

        let expected = local.id().to_string();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].0, "vid-1");
        assert_eq!(decisions[0].1.target(), Some(expected.as_str()));
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn shadow_ignores_local_sources(pool: sqlx::PgPool) {
        let store = Arc::new(PostgresTrackRepository::new(pool));
        crate::features::music_library::storage::contracts::CatalogueWrite::get_or_create(
            store.as_ref(),
            local_source(),
            metadata("Song"),
        )
        .await
        .unwrap();
        let track = Track::new(TrackId::new(), metadata("Song"));

        let decisions = shadow(
            &(Arc::clone(&store) as Arc<dyn LibraryRead>),
            &(Arc::clone(&store) as Arc<dyn SourceLookup>),
            &[hit(
                track,
                SourceIdentity::new(
                    ProviderId::new("local".to_owned()).unwrap(),
                    SourceScope::Named("/music".to_owned()),
                    "path-b".to_owned(),
                )
                .unwrap(),
            )],
            &context(),
        )
        .await;

        assert!(decisions.is_empty());
    }
    struct UnusableCandidates {
        hits: Vec<TrackHit>,
        delay: bool,
        fail: bool,
    }

    #[async_trait::async_trait]
    impl crate::features::music_library::storage::contracts::LibraryRead for UnusableCandidates {
        async fn find_track(
            &self,
            _: &crate::features::music_library::domain::TrackId,
        ) -> Result<
            Option<crate::features::music_library::domain::Track>,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Ok(None)
        }
        async fn dedup_candidates(
            &self,
            _: &[String],
            _: u32,
        ) -> Result<
            Vec<TrackHit>,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            if self.delay {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            if self.fail {
                return Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable);
            }
            Ok(self.hits.clone())
        }
        async fn page_tracks(
            &self,
            _: u32,
            _: u32,
            _: crate::features::music_library::domain::TrackOrder,
            _: &crate::features::music_library::domain::TrackListFilter,
            _: Option<uuid::Uuid>,
        ) -> Result<
            crate::features::music_library::domain::TrackPage,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }
        async fn track_views(
            &self,
            _: &[crate::features::music_library::domain::TrackId],
            _: Option<uuid::Uuid>,
        ) -> Result<
            Vec<crate::features::music_library::domain::TrackView>,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }
    }
    #[async_trait::async_trait]
    impl crate::features::music_library::storage::contracts::CatalogueWrite for UnusableCandidates {
        async fn get_or_create(
            &self,
            _: crate::features::music_library::domain::SourceIdentity,
            _: crate::features::music_library::domain::TrackMetadata,
        ) -> Result<
            crate::features::music_library::domain::Track,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }

        async fn get_or_create_provider(
            &self,
            _: crate::features::music_library::domain::SourceIdentity,
            _: crate::features::music_library::domain::TrackMetadata,
            _album_id: Option<&crate::features::music_library::domain::AlbumId>,
            _: &[crate::features::music_library::domain::SourceIdentity],
        ) -> Result<
            crate::features::music_library::domain::Track,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }

        async fn refresh_album_facts(
            &self,
            _: &crate::features::music_library::domain::AlbumId,
            _: &crate::features::music_library::domain::AlbumMetadata,
        ) -> Result<
            crate::features::music_library::domain::Album,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }

        async fn add_album_track(
            &self,
            _: &crate::features::music_library::domain::AlbumId,
            _: &crate::features::music_library::domain::TrackId,
            _: &crate::features::music_library::domain::AlbumTrack,
        ) -> Result<(), crate::features::music_library::storage::contracts::TrackRepositoryError>
        {
            Err(crate::features::music_library::storage::contracts::TrackRepositoryError::Unavailable)
        }
    }
    #[async_trait::async_trait]
    impl crate::features::music_library::storage::contracts::SourceLookup for UnusableCandidates {
        async fn find_sources(
            &self,
            _: &crate::features::music_library::domain::TrackId,
        ) -> Result<
            Vec<crate::features::music_library::domain::SourceIdentity>,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Ok(vec![])
        }

        async fn set_album_source_artwork(
            &self,
            _: &crate::features::music_library::domain::SourceIdentity,
            _: &crate::features::music_library::domain::ArtworkRef,
        ) -> Result<(), crate::features::music_library::storage::contracts::TrackRepositoryError>
        {
            Ok(())
        }

        async fn find_album_artwork(
            &self,
            _: &crate::features::music_library::domain::AlbumId,
        ) -> Result<
            Option<(
                crate::features::music_library::domain::SourceIdentity,
                crate::features::music_library::domain::ArtworkRef,
            )>,
            crate::features::music_library::storage::contracts::TrackRepositoryError,
        > {
            Ok(None)
        }
    }
    impl crate::features::music_library::storage::contracts::StateStore for UnusableCandidates {}
    impl crate::features::music_library::storage::contracts::DuplicateReport for UnusableCandidates {}

    #[tokio::test]
    async fn suppression_keeps_remote_results_on_overflow_error_or_timeout() {
        let metadata = crate::features::music_library::domain::TrackMetadata::from_names(
            "Song".into(),
            vec!["Artist".into()],
            Some(240000),
        )
        .unwrap();
        let remote = TrackHit::new(
            crate::features::music_library::domain::Track::new(
                crate::features::music_library::domain::TrackId::new(),
                metadata,
            ),
            None,
            vec![provider_source()],
        );
        for (delay, fail, count) in [(false, false, 501), (false, true, 0), (true, false, 0)] {
            let store = Arc::new(UnusableCandidates {
                hits: vec![remote.clone(); count],
                delay,
                fail,
            });
            let started = std::time::Instant::now();
            let library = Arc::clone(&store) as Arc<dyn LibraryRead>;
            let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
            let state = Arc::clone(&store) as Arc<dyn StateStore>;
            let (known, fresh) = apply(
                Ports {
                    library: &library,
                    sources: &sources,
                    state: &state,
                },
                vec![remote.clone()],
                vec![],
                None,
                &context(),
                DedupMode::Suppress,
                DEFAULT_DEDUP_BUDGET,
            )
            .await;
            assert_eq!(known[0].track().id(), remote.track().id());
            assert!(fresh.is_empty());
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
        }
    }
}
