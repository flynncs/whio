use std::path::Path;

use axum::{
    body::Body,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{CACHE_CONTROL, CONTENT_TYPE, ETAG},
    },
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use md5::{Digest, Md5};
use uuid::Uuid;

use crate::{
    AppState,
    media::{self, MediaFetchError},
    playback::{PlaybackError, ResolvedArtwork},
    request::RequestContext,
    tracks::CoverArtTarget,
};

const CACHE_CONTROL_VALUE: &str = "public, max-age=86400";

pub(crate) enum ArtworkFailure {
    InvalidId(uuid::Error),
    Playback(PlaybackError),
    Fetch(MediaFetchError),
}

pub(crate) enum ArtworkOutcome {
    Found(Artwork),
    NotModified(String),
    Missing,
}

pub(crate) struct Artwork {
    content_type: String,
    bytes: Bytes,
    etag: String,
}

impl Artwork {
    fn new(content_type: String, bytes: Bytes, etag: String) -> Self {
        Self {
            content_type,
            bytes,
            etag,
        }
    }

    pub(crate) fn into_response(self) -> Response {
        let mut headers = validators(&self.etag);
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&self.content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );

        (headers, Body::from(self.bytes)).into_response()
    }

    pub(crate) fn not_modified(etag: String) -> Response {
        (StatusCode::NOT_MODIFIED, validators(&etag)).into_response()
    }
}

fn validators(etag: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        ETAG,
        HeaderValue::from_str(etag).expect("an etag is a valid header value"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE));

    headers
}

pub(crate) async fn load(
    state: &AppState,
    context: &RequestContext,
    raw_id: &str,
    user: &Uuid,
    if_none_match: Option<&str>,
) -> Result<ArtworkOutcome, ArtworkFailure> {
    let target = CoverArtTarget::parse(raw_id).map_err(ArtworkFailure::InvalidId)?;
    let resolved = match target {
        CoverArtTarget::Track(track_id) => {
            state
                .playback()
                .artwork(&track_id, Some(user), context)
                .await
        }
        CoverArtTarget::Album(album_id) => {
            state
                .playback()
                .album_artwork(&album_id, Some(user), context)
                .await
        }
        CoverArtTarget::Artist(artist_id) => {
            state
                .playback()
                .artist_artwork(&artist_id, Some(user), context)
                .await
        }
    }
    .map_err(ArtworkFailure::Playback)?;

    let Some(resolved) = resolved else {
        return Ok(ArtworkOutcome::Missing);
    };

    match resolved {
        ResolvedArtwork::Registered(url) => {
            let url = parse(&url)?;
            fetch(state, &url, etag_for(url.as_str()), if_none_match).await
        }
        ResolvedArtwork::Public(url) => {
            let url = parse(&url)?;
            if !media::is_allowed_artwork_url(&url, state.artwork_host_suffixes()) {
                return Ok(ArtworkOutcome::Missing);
            }
            fetch(state, &url, etag_for(url.as_str()), if_none_match).await
        }
        ResolvedArtwork::Local { path, picture } => {
            let metadata = tokio::fs::metadata(&path).await.ok();
            let etag = local_etag(&path, metadata.as_ref());
            if fresh(if_none_match, &etag) {
                return Ok(ArtworkOutcome::NotModified(etag));
            }

            Ok(ArtworkOutcome::Found(Artwork::new(
                picture.content_type,
                Bytes::from(picture.bytes),
                etag,
            )))
        }
    }
}

async fn fetch(
    state: &AppState,
    url: &reqwest::Url,
    etag: String,
    if_none_match: Option<&str>,
) -> Result<ArtworkOutcome, ArtworkFailure> {
    if fresh(if_none_match, &etag) {
        return Ok(ArtworkOutcome::NotModified(etag));
    }

    let (content_type, bytes) = media::fetch_artwork(state.artwork_client(), url)
        .await
        .map_err(ArtworkFailure::Fetch)?;

    Ok(ArtworkOutcome::Found(Artwork::new(
        content_type,
        bytes,
        etag,
    )))
}

fn fresh(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(header) = if_none_match else {
        return false;
    };

    header.split(',').any(|tag| {
        let tag = tag.trim();
        tag == "*" || tag.trim_start_matches("W/") == etag
    })
}

fn etag_for(value: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(value.as_bytes());

    format!("\"{:x}\"", hasher.finalize())
}

fn local_etag(path: &Path, metadata: Option<&std::fs::Metadata>) -> String {
    let mut basis = format!("{}:", path.display());
    if let Some(metadata) = metadata {
        basis.push_str(&metadata.len().to_string());
        if let Ok(modified) = metadata.modified() {
            basis.push(':');
            basis.push_str(&chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339());
        }
    }

    etag_for(&basis)
}

fn parse(url: &str) -> Result<reqwest::Url, ArtworkFailure> {
    reqwest::Url::parse(url)
        .map_err(|_| ArtworkFailure::Fetch(MediaFetchError::DisallowedDestination))
}
