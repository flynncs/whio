use std::path::{Path, PathBuf};

use chrono::Utc;
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::features::acquisition::ports::{
    CandidateFile, CandidateLimit, FileFacts, PeerFacts, Rejection, ReleaseCandidate, SearchText,
    Slot, TransferError, TransferHandle, TransferInput, TransferPhase, TransferProgress,
    TransferRequest,
};

use super::Slskd;

const API_KEY_HEADER: &str = "X-API-Key";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest<'a> {
    id: Uuid,
    search_text: &'a str,
    search_timeout: u64,
    file_limit: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchState {
    #[serde(default)]
    is_complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeerResponse {
    username: String,
    #[serde(default)]
    has_free_upload_slot: Option<bool>,
    #[serde(default)]
    queue_length: Option<u32>,
    #[serde(default)]
    upload_speed: Option<u64>,
    #[serde(default)]
    files: Vec<RemoteFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteFile {
    filename: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    extension: Option<String>,
    #[serde(default)]
    bit_rate: Option<u32>,
    #[serde(default)]
    sample_rate: Option<u32>,
    #[serde(default)]
    bit_depth: Option<u32>,
    #[serde(default)]
    length: Option<u32>,
    #[serde(default)]
    is_locked: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchRequest<'a> {
    id: Uuid,
    username: &'a str,
    files: Vec<BatchFile<'a>>,
}

#[derive(Serialize)]
struct BatchFile<'a> {
    filename: &'a str,
    size: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Batch {
    username: String,
    #[serde(default)]
    transfers: Vec<BatchTransfer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchTransfer {
    id: Uuid,
    filename: String,
    #[serde(default)]
    size: Option<u64>,
    state: String,
    #[serde(default)]
    bytes_transferred: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchResponse {
    #[serde(default)]
    failures: Vec<BatchFailure>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchFailure {
    filename: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ServerState {
    #[serde(default)]
    is_logged_in: bool,
}

impl Slskd {
    pub(super) async fn create_search(
        &self,
        id: Uuid,
        query: &SearchText,
        limit: CandidateLimit,
    ) -> Result<(), TransferError> {
        let request = SearchRequest {
            id,
            search_text: query.as_str(),
            search_timeout: u64::try_from(self.search_timeout.as_millis()).unwrap_or(u64::MAX),
            file_limit: limit.get(),
        };
        let response = self
            .send(
                self.client
                    .post(self.url("api/v0/searches")?)
                    .json(&request),
            )
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => Ok(()),
            StatusCode::BAD_REQUEST => Err(TransferError::InvalidRequest),
            _ => Err(TransferError::Internal),
        }
    }

    pub(super) async fn search_complete(&self, id: Uuid) -> Result<bool, TransferError> {
        let response = self
            .send(self.client.get(self.url(&format!("api/v0/searches/{id}"))?))
            .await?;

        match response.status() {
            StatusCode::OK => Ok(read::<SearchState>(response).await?.is_complete),
            StatusCode::NOT_FOUND => Err(TransferError::Internal),
            _ => Err(TransferError::Internal),
        }
    }

    pub(super) async fn search_candidates(
        &self,
        id: Uuid,
    ) -> Result<Vec<ReleaseCandidate>, TransferError> {
        let response = self
            .send(
                self.client
                    .get(self.url(&format!("api/v0/searches/{id}/responses"))?),
            )
            .await?;

        match response.status() {
            StatusCode::OK => Ok(group_candidates(read::<Vec<PeerResponse>>(response).await?)),
            _ => Err(TransferError::Malformed),
        }
    }

    pub(super) async fn delete_search(&self, id: Uuid) {
        let result = match self.url(&format!("api/v0/searches/{id}")) {
            Ok(url) => self.send(self.client.delete(url)).await,
            Err(error) => Err(error),
        };

        match result {
            Ok(response)
                if !response.status().is_success()
                    && response.status() != StatusCode::NOT_FOUND =>
            {
                tracing::debug!(
                    status = response.status().as_u16(),
                    "the search could not be deleted"
                );
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "the search could not be deleted"),
        }
    }

    pub(super) async fn enqueue(
        &self,
        request: &TransferRequest,
    ) -> Result<Slot<TransferHandle>, TransferError> {
        let TransferInput::SoulseekFile {
            peer,
            path,
            size_bytes,
        } = &request.input;
        let body = BatchRequest {
            id: request.key,
            username: peer,
            files: vec![BatchFile {
                filename: path,
                size: *size_bytes,
            }],
        };
        let response = self
            .send(
                self.client
                    .post(self.url("api/v0/transfers/downloads/batches")?)
                    .json(&body),
            )
            .await?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => {
                Ok(Slot::Accepted(self.handle(request.key, peer, path)))
            }
            StatusCode::MULTI_STATUS => {
                let failures = read::<BatchResponse>(response).await?.failures;

                if failures.iter().any(|failure| failure.filename == *path) {
                    Ok(Slot::Rejected(Rejection::Refused))
                } else {
                    Ok(Slot::Accepted(self.handle(request.key, peer, path)))
                }
            }
            StatusCode::BAD_REQUEST => Ok(Slot::Rejected(Rejection::Invalid)),
            StatusCode::CONFLICT => self.reconcile(request.key, peer, path).await,
            _ => Err(TransferError::Internal),
        }
    }

    pub(super) async fn batch_progress(
        &self,
        handle: &TransferHandle,
    ) -> Result<Slot<TransferProgress>, TransferError> {
        let Some((id, peer, path)) = parse_handle(handle) else {
            return Ok(Slot::Rejected(Rejection::Invalid));
        };
        let Some(batch) = self.batch(id).await? else {
            return Ok(Slot::Missing);
        };
        let transfer = matched_transfer(&batch, &peer, &path)?;
        let phase = parse_phase(&transfer.state);
        let delivered =
            (phase == TransferPhase::ReportedComplete).then(|| delivered_path(&transfer.filename));

        Ok(Slot::Accepted(TransferProgress {
            phase,
            bytes_done: transfer.bytes_transferred,
            bytes_total: transfer.size,
            observed_at: Utc::now(),
            delivered,
        }))
    }

    pub(super) async fn cancel_transfer(
        &self,
        handle: &TransferHandle,
        remove: bool,
    ) -> Result<(), TransferError> {
        let Some((id, peer, path)) = parse_handle(handle) else {
            return Err(TransferError::InvalidRequest);
        };
        let Some(batch) = self.batch(id).await? else {
            return Ok(());
        };
        let transfer = matched_transfer(&batch, &peer, &path)?;

        let mut url = self.url("api/v0/transfers/downloads")?;
        url.path_segments_mut()
            .map_err(|_| TransferError::Malformed)?
            .push(&peer)
            .push(&transfer.id.to_string());
        url.query_pairs_mut()
            .append_pair("remove", if remove { "true" } else { "false" });

        let response = self.send(self.client.delete(url)).await?;

        match response.status() {
            StatusCode::NO_CONTENT | StatusCode::NOT_FOUND => Ok(()),
            _ => Err(TransferError::Internal),
        }
    }

    pub(super) async fn logged_in(&self) -> Result<bool, TransferError> {
        let response = self
            .send(self.client.get(self.url("api/v0/server")?))
            .await?;

        match response.status() {
            StatusCode::OK => Ok(read::<ServerState>(response).await?.is_logged_in),
            _ => Err(TransferError::Internal),
        }
    }

    async fn reconcile(
        &self,
        key: Uuid,
        peer: &str,
        path: &str,
    ) -> Result<Slot<TransferHandle>, TransferError> {
        let Some(batch) = self.batch(key).await? else {
            return Ok(Slot::Rejected(Rejection::Conflict));
        };
        matched_transfer(&batch, peer, path)?;

        Ok(Slot::Accepted(self.handle(key, peer, path)))
    }

    async fn batch(&self, id: Uuid) -> Result<Option<Batch>, TransferError> {
        let response = self
            .send(
                self.client
                    .get(self.url(&format!("api/v0/transfers/downloads/batches/{id}"))?),
            )
            .await?;

        match response.status() {
            StatusCode::OK => Ok(Some(read::<Batch>(response).await?)),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(TransferError::Internal),
        }
    }

    async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response, TransferError> {
        let response = request
            .header(API_KEY_HEADER, &self.api_key)
            .send()
            .await
            .map_err(|_| TransferError::Unavailable)?;
        let status = response.status();

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(TransferError::CredentialsRejected);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(TransferError::Unavailable);
        }

        Ok(response)
    }

    fn url(&self, path: &str) -> Result<reqwest::Url, TransferError> {
        self.base.join(path).map_err(|_| TransferError::Malformed)
    }

    fn handle(&self, key: Uuid, peer: &str, path: &str) -> TransferHandle {
        TransferHandle::new(format!("{key}\u{1f}{peer}\u{1f}{path}"))
    }
}

fn parse_handle(handle: &TransferHandle) -> Option<(Uuid, String, String)> {
    let mut fields = handle.as_str().split('\u{1f}');
    let id = Uuid::parse_str(fields.next()?).ok()?;
    let peer = fields.next()?;
    let path = fields.next()?;

    if peer.is_empty() || path.is_empty() || fields.next().is_some() {
        return None;
    }

    Some((id, peer.to_owned(), path.to_owned()))
}

fn matched_transfer<'a>(
    batch: &'a Batch,
    peer: &str,
    path: &str,
) -> Result<&'a BatchTransfer, TransferError> {
    if batch.username != peer || batch.transfers.len() != 1 {
        return Err(TransferError::Malformed);
    }
    let transfer = &batch.transfers[0];

    if transfer.filename != path {
        return Err(TransferError::Malformed);
    }

    Ok(transfer)
}

async fn read<T: DeserializeOwned>(response: Response) -> Result<T, TransferError> {
    response
        .json::<T>()
        .await
        .map_err(|_| TransferError::Malformed)
}

fn group_candidates(responses: Vec<PeerResponse>) -> Vec<ReleaseCandidate> {
    let mut candidates: Vec<ReleaseCandidate> = Vec::new();

    for response in responses {
        let peer = PeerFacts {
            queue_length: response.queue_length,
            upload_speed: response.upload_speed,
            has_free_slot: response.has_free_upload_slot,
        };

        for file in response.files {
            if file.is_locked {
                continue;
            }
            let (directory, name) = split_remote(&file.filename);
            let source = format!("{}:{}", response.username, directory);

            match candidates
                .iter_mut()
                .find(|candidate| candidate.source == source)
            {
                Some(candidate) => {
                    candidate
                        .files
                        .push(candidate_file(&response.username, name.to_owned(), file))
                }
                None => candidates.push(ReleaseCandidate {
                    source,
                    peer: peer.clone(),
                    files: vec![candidate_file(&response.username, name.to_owned(), file)],
                }),
            }
        }
    }

    candidates
}

fn candidate_file(peer: &str, name: String, file: RemoteFile) -> CandidateFile {
    let size_bytes = file.size.unwrap_or_default();

    CandidateFile {
        input: TransferInput::SoulseekFile {
            peer: peer.to_owned(),
            path: file.filename.clone(),
            size_bytes,
        },
        name,
        facts: FileFacts {
            size_bytes: file.size,
            extension: file.extension,
            bitrate_kbps: file.bit_rate,
            sample_rate_hz: file.sample_rate,
            bit_depth: file.bit_depth,
            duration_seconds: file.length,
        },
    }
}

fn split_remote(filename: &str) -> (&str, &str) {
    match filename.rfind(['\\', '/']) {
        Some(index) => (&filename[..index], &filename[index + 1..]),
        None => ("", filename),
    }
}

fn parse_phase(state: &str) -> TransferPhase {
    let flags = state.split(',').map(str::trim).collect::<Vec<_>>();
    let has = |flag: &str| flags.iter().any(|value| value.eq_ignore_ascii_case(flag));

    if has("Succeeded") {
        return TransferPhase::ReportedComplete;
    }
    if has("Cancelled") {
        return TransferPhase::Cancelled;
    }
    if ["TimedOut", "Errored", "Rejected", "Aborted"]
        .iter()
        .any(|flag| has(flag))
    {
        return TransferPhase::Failed;
    }
    if has("InProgress") || has("Initializing") {
        return TransferPhase::Active;
    }

    TransferPhase::Queued
}

fn delivered_path(remote_filename: &str) -> PathBuf {
    let localized = remote_filename.replace('\\', "/");
    let unrooted = strip_root(&localized);
    let mut segments = unrooted
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(sanitize)
        .collect::<Vec<_>>();

    match segments.pop() {
        Some(file) => match segments.last() {
            Some(directory) => Path::new(directory).join(file),
            None => PathBuf::from(file),
        },
        None => PathBuf::new(),
    }
}

fn strip_root(path: &str) -> &str {
    strip_drive_root(path)
        .or_else(|| strip_unc_root(path))
        .or_else(|| strip_qt_root(path))
        .unwrap_or(path)
}

fn strip_drive_root(path: &str) -> Option<&str> {
    let bytes = path.as_bytes();

    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(path[2..].strip_prefix('/').unwrap_or(&path[2..]));
    }

    None
}

fn strip_unc_root(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("//")?;
    let (_, rest) = rest.split_once('/')?;

    Some(rest)
}

fn strip_qt_root(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("@@")?;
    let length = rest
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .count();

    if length < 5 {
        return None;
    }

    Some(rest[length..].strip_prefix('/').unwrap_or(&rest[length..]))
}

fn sanitize(segment: &str) -> String {
    segment
        .chars()
        .map(|character| match character {
            '\0' | '/' | '\\' => '_',
            other => other,
        })
        .collect()
}
