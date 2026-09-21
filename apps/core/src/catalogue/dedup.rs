use std::{collections::BTreeSet, sync::Arc};

use crate::{
    request::RequestContext,
    tracks::{MatchDecision, MatchRecord, ProviderKind, RecordingIndex, TrackHit, TrackRepository},
};

const CANDIDATE_LIMIT: u32 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupMode {
    Off,
    Shadow,
    Suppress,
}

const SUPPRESSION_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

pub(crate) async fn apply(
    repository: &Arc<dyn TrackRepository>,
    mut known: Vec<TrackHit>,
    mut fresh: Vec<TrackHit>,
    user: Option<uuid::Uuid>,
    context: &RequestContext,
    mode: DedupMode,
) -> (Vec<TrackHit>, Vec<TrackHit>) {
    if mode == DedupMode::Off {
        return (known, fresh);
    }
    let started = std::time::Instant::now();
    let hits = known.iter().chain(&fresh).cloned().collect::<Vec<_>>();
    if mode == DedupMode::Shadow {
        let _ = tokio::time::timeout(SUPPRESSION_BUDGET, shadow(repository, &hits, context)).await;
        return (known, fresh);
    }
    match tokio::time::timeout(SUPPRESSION_BUDGET, replacements(repository, &hits, user)).await {
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
    repository: &Arc<dyn TrackRepository>,
    hits: &[TrackHit],
    user: Option<uuid::Uuid>,
) -> Result<
    std::collections::HashMap<crate::tracks::TrackId, TrackHit>,
    crate::tracks::TrackRepositoryError,
> {
    use crate::tracks::{MatchStatus, normalize_for_matching};
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
    let candidates = repository
        .dedup_candidates(&keys, CANDIDATE_LIMIT + 1)
        .await?;
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
            let ready = if local_available(repository, local).await? {
                let mut local = local.clone();
                if let Some(user) = user {
                    let state = repository
                        .find_track_state(local.track().id(), user)
                        .await?;
                    local = local.with_state(state);
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
    repository: &Arc<dyn TrackRepository>,
    hit: &TrackHit,
) -> Result<bool, crate::tracks::TrackRepositoryError> {
    for source in hit.sources() {
        if source.provider_id().kind() != Some(ProviderKind::Local) {
            continue;
        }
        let crate::tracks::SourceScope::Named(root) = source.source_scope() else {
            continue;
        };
        let Some(facts) = repository.find_source_file_facts(source).await? else {
            continue;
        };
        if facts.missing_at.is_some() {
            continue;
        }
        let Some(relative) = facts.path.as_deref() else {
            continue;
        };
        let Some(path) = crate::source_path::local_path(root, relative) else {
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
    repository: &Arc<dyn TrackRepository>,
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

    let candidates = match repository.dedup_candidates(&keys, CANDIDATE_LIMIT).await {
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
    use crate::{
        request::RequestId,
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, Track, TrackId,
            TrackMetadata,
        },
    };

    fn provider_source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "vid-1".to_owned(),
        )
        .unwrap()
    }

    fn local_source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/music".to_owned()),
            "path-a".to_owned(),
        )
        .unwrap()
    }

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::from_names(title.to_owned(), vec!["A".to_owned()], Some(240_000)).unwrap()
    }

    fn hit(track: Track, source: SourceIdentity) -> TrackHit {
        TrackHit::new(track, None, vec![source])
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-1".to_owned()).unwrap())
    }

    #[tokio::test]
    async fn shadow_matches_a_provider_hit_against_a_local_track() {
        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());
        let local = repository
            .get_or_create(local_source(), metadata("Song"))
            .await
            .unwrap();
        let remote = Track::new(TrackId::new(), metadata("Song"));

        let decisions = shadow(&repository, &[hit(remote, provider_source())], &context()).await;

        let expected = local.id().to_string();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].0, "vid-1");
        assert_eq!(decisions[0].1.target(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn shadow_ignores_local_sources() {
        let repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());
        repository
            .get_or_create(local_source(), metadata("Song"))
            .await
            .unwrap();
        let track = Track::new(TrackId::new(), metadata("Song"));

        let decisions = shadow(
            &repository,
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
    impl crate::tracks::LibraryRead for UnusableCandidates {
        async fn find_track(
            &self,
            _: &crate::tracks::TrackId,
        ) -> Result<Option<crate::tracks::Track>, crate::tracks::TrackRepositoryError> {
            Ok(None)
        }
        async fn dedup_candidates(
            &self,
            _: &[String],
            _: u32,
        ) -> Result<Vec<TrackHit>, crate::tracks::TrackRepositoryError> {
            if self.delay {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            if self.fail {
                return Err(crate::tracks::TrackRepositoryError::Unavailable);
            }
            Ok(self.hits.clone())
        }
    }
    #[async_trait::async_trait]
    impl crate::tracks::CatalogueWrite for UnusableCandidates {
        async fn get_or_create(
            &self,
            _: crate::tracks::SourceIdentity,
            _: crate::tracks::TrackMetadata,
        ) -> Result<crate::tracks::Track, crate::tracks::TrackRepositoryError> {
            Err(crate::tracks::TrackRepositoryError::Unavailable)
        }
    }
    #[async_trait::async_trait]
    impl crate::tracks::SourceLookup for UnusableCandidates {
        async fn find_sources(
            &self,
            _: &crate::tracks::TrackId,
        ) -> Result<Vec<crate::tracks::SourceIdentity>, crate::tracks::TrackRepositoryError>
        {
            Ok(vec![])
        }
    }
    impl crate::tracks::StateStore for UnusableCandidates {}
    impl crate::tracks::DuplicateReport for UnusableCandidates {}

    #[tokio::test]
    async fn suppression_keeps_remote_results_on_overflow_error_or_timeout() {
        let metadata = crate::tracks::TrackMetadata::from_names(
            "Song".into(),
            vec!["Artist".into()],
            Some(240000),
        )
        .unwrap();
        let remote = TrackHit::new(
            crate::tracks::Track::new(crate::tracks::TrackId::new(), metadata),
            None,
            vec![provider_source()],
        );
        for (delay, fail, count) in [(false, false, 501), (false, true, 0), (true, false, 0)] {
            let repository: Arc<dyn TrackRepository> = Arc::new(UnusableCandidates {
                hits: vec![remote.clone(); count],
                delay,
                fail,
            });
            let started = std::time::Instant::now();
            let (known, fresh) = apply(
                &repository,
                vec![remote.clone()],
                vec![],
                None,
                &context(),
                DedupMode::Suppress,
            )
            .await;
            assert_eq!(known[0].track().id(), remote.track().id());
            assert!(fresh.is_empty());
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
        }
    }
}
