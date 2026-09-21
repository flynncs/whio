use std::{collections::BTreeSet, sync::Arc};

use crate::{
    request::RequestContext,
    tracks::{MatchDecision, MatchRecord, ProviderKind, RecordingIndex, TrackHit, TrackRepository},
};

const CANDIDATE_LIMIT: u32 = 500;

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
}
