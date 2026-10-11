use async_trait::async_trait;
use serde_json::Value;

use crate::features::acquisition::ports::{
    ExpectedTrack, LookupError, MbId, MbRecording, MbRelease, MusicBrainzLookup,
};
use crate::features::metadata::refresh::DocumentError;

use super::client::MusicBrainzClient;

const RELEASE_CANDIDATES: u16 = 5;
const RECORDING_CANDIDATES: u16 = 10;

#[async_trait]
impl MusicBrainzLookup for MusicBrainzClient {
    #[tracing::instrument(skip_all, fields(backend = "musicbrainz", kind = "release", limit = RELEASE_CANDIDATES))]
    async fn release_candidates(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRelease>, LookupError> {
        let search = self
            .search_releases(artist, title, RELEASE_CANDIDATES)
            .await
            .map_err(lookup)?;
        let mut releases = Vec::new();

        for id in release_ids(&search)? {
            match self.fetch_release(&id.to_string()).await {
                Ok(value) => releases.push(parse_release(&value)?),
                Err(DocumentError::NotFound) => {}
                Err(error) => return Err(lookup(error)),
            }
        }

        Ok(releases)
    }

    #[tracing::instrument(skip_all, fields(backend = "musicbrainz", kind = "release"))]
    async fn release(&self, id: &MbId) -> Result<Option<MbRelease>, LookupError> {
        match self.fetch_release(&id.to_string()).await {
            Ok(value) => parse_release(&value).map(Some),
            Err(DocumentError::NotFound) => Ok(None),
            Err(error) => Err(lookup(error)),
        }
    }

    #[tracing::instrument(skip_all, fields(backend = "musicbrainz", kind = "recording", limit = RECORDING_CANDIDATES))]
    async fn recording_candidates(
        &self,
        artist: &str,
        title: &str,
    ) -> Result<Vec<MbRecording>, LookupError> {
        let search = self
            .search_recordings(artist, title, RECORDING_CANDIDATES)
            .await
            .map_err(lookup)?;
        let entries = search
            .get("recordings")
            .and_then(Value::as_array)
            .ok_or(LookupError::Malformed)?;

        entries
            .iter()
            .take(usize::from(RECORDING_CANDIDATES))
            .map(parse_recording)
            .collect()
    }

    #[tracing::instrument(skip_all, fields(backend = "musicbrainz", kind = "recording"))]
    async fn recording(&self, id: &MbId) -> Result<Option<MbRecording>, LookupError> {
        match self.fetch_recording(&id.to_string()).await {
            Ok(value) => parse_recording(&value).map(Some),
            Err(DocumentError::NotFound) => Ok(None),
            Err(error) => Err(lookup(error)),
        }
    }
}

fn lookup(error: DocumentError) -> LookupError {
    match error {
        DocumentError::Unavailable { .. } => LookupError::Unavailable,
        _ => LookupError::Malformed,
    }
}

fn release_ids(value: &Value) -> Result<Vec<MbId>, LookupError> {
    let entries = value
        .get("releases")
        .and_then(Value::as_array)
        .ok_or(LookupError::Malformed)?;

    entries
        .iter()
        .take(usize::from(RELEASE_CANDIDATES))
        .map(|entry| mb_id(entry).ok_or(LookupError::Malformed))
        .collect()
}

fn parse_release(value: &Value) -> Result<MbRelease, LookupError> {
    let id = mb_id(value).ok_or(LookupError::Malformed)?;
    let title = string(value, "title").ok_or(LookupError::Malformed)?;
    let artist = artist_credit(value);
    let media = value
        .get("media")
        .and_then(Value::as_array)
        .ok_or(LookupError::Malformed)?;
    let mut tracks = Vec::new();

    for medium in media {
        let disc = medium
            .get("position")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let disc = u16::try_from(disc).map_err(|_| LookupError::Malformed)?;

        for track in medium
            .get("tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let position = track
                .get("position")
                .and_then(Value::as_u64)
                .or_else(|| track.get("number").and_then(Value::as_u64))
                .ok_or(LookupError::Malformed)?;
            let position = u16::try_from(position).map_err(|_| LookupError::Malformed)?;
            tracks.push(ExpectedTrack {
                disc,
                position,
                title: string(track, "title").ok_or(LookupError::Malformed)?,
                length_ms: length_ms(track),
            });
        }
    }

    tracks.sort_by_key(|track| (track.disc, track.position));

    Ok(MbRelease {
        id,
        artist,
        title,
        tracks,
    })
}

fn parse_recording(value: &Value) -> Result<MbRecording, LookupError> {
    let id = mb_id(value).ok_or(LookupError::Malformed)?;

    Ok(MbRecording {
        id,
        artist: artist_credit(value),
        title: string(value, "title").ok_or(LookupError::Malformed)?,
        length_ms: length_ms(value),
    })
}

fn artist_credit(value: &Value) -> String {
    value
        .get("artist-credit")
        .and_then(Value::as_array)
        .and_then(|credits| credits.first())
        .and_then(|credit| credit.get("name").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

fn length_ms(value: &Value) -> Option<u32> {
    value
        .get("length")
        .and_then(Value::as_u64)
        .and_then(|length| u32::try_from(length).ok())
}

fn string(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn mb_id(value: &Value) -> Option<MbId> {
    string(value, "id").as_deref().and_then(MbId::parse)
}
