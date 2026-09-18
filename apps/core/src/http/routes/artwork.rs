use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use uuid::Uuid;

use crate::{
    AppState,
    media::{self, MediaFetchError},
    playback::{ArtworkLocation, PlaybackError},
    request::RequestContext,
    tracks::TrackId,
};

pub(crate) enum ArtworkFailure {
    InvalidId(uuid::Error),
    Playback(PlaybackError),
    Fetch(MediaFetchError),
}

pub(crate) struct Artwork {
    content_type: String,
    bytes: Bytes,
}

impl Artwork {
    pub(crate) fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_str(&self.content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );

        (headers, Body::from(self.bytes)).into_response()
    }
}

/// resolve and fetch a track's cover art. absent artwork is not an error.
pub(crate) async fn load(
    state: &AppState,
    context: &RequestContext,
    raw_id: &str,
    user: &Uuid,
) -> Result<Option<Artwork>, ArtworkFailure> {
    let track_id = raw_id
        .parse::<TrackId>()
        .map_err(ArtworkFailure::InvalidId)?;

    let Some(location) = state
        .playback()
        .artwork(&track_id, Some(user), context)
        .await
        .map_err(ArtworkFailure::Playback)?
    else {
        return Ok(None);
    };

    let url = match location {
        ArtworkLocation::Registered(url) => parse(&url)?,
        ArtworkLocation::Public(url) => {
            let url = parse(&url)?;
            if !media::is_allowed_artwork_url(&url, state.artwork_host_suffixes()) {
                return Ok(None);
            }
            url
        }
    };

    let (content_type, bytes) = media::fetch_artwork(state.artwork_client(), &url)
        .await
        .map_err(ArtworkFailure::Fetch)?;

    Ok(Some(Artwork {
        content_type,
        bytes,
    }))
}

fn parse(url: &str) -> Result<reqwest::Url, ArtworkFailure> {
    reqwest::Url::parse(url)
        .map_err(|_| ArtworkFailure::Fetch(MediaFetchError::DisallowedDestination))
}
