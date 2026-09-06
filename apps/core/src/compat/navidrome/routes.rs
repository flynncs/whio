use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::{
    AppState,
    catalogue::{CatalogueSearch, SearchLimit, SearchQuery},
    request::RequestContext,
    tracks::Track,
};

use super::{
    auth::NdToken,
    dto::{Song, SongListQuery},
    errors, session,
};

impl From<Track> for Song {
    fn from(track: Track) -> Self {
        Self {
            id: track.id().to_string(),
            title: track.title().to_owned(),
            artist: track.artists().join(", "),
            duration: track
                .duration_ms()
                .map(|ms| ms as f32 / 1000.0)
                .unwrap_or(0.0),
        }
    }
}

fn filter_q(filter: Option<&str>) -> String {
    let Some(raw) = filter else {
        return String::new();
    };
    let Some(key) = raw.find("\"q\"") else {
        return String::new();
    };
    let Some(colon) = raw[key + 3..].find(':') else {
        return String::new();
    };
    let mut chars = raw[key + 3 + colon + 1..].trim_start().chars();
    if chars.next() != Some('"') {
        return String::new();
    }
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            break;
        } else {
            out.push(c);
        }
    }
    out
}

fn result_limit(range: Option<&str>) -> u32 {
    const DEFAULT: u32 = 25;
    let Some(count) = range.and_then(|raw| {
        let inner = raw.trim().strip_prefix('[')?.strip_suffix(']')?;
        let (start, end) = inner.split_once(',')?;
        let (start, end): (u32, u32) = (start.trim().parse().ok()?, end.trim().parse().ok()?);
        end.checked_sub(start)?.checked_add(1)
    }) else {
        return DEFAULT;
    };
    count.clamp(1, 25)
}

pub(crate) async fn song_list(
    State(state): State<AppState>,
    Extension(context): Extension<RequestContext>,
    token: NdToken,
    Query(query): Query<SongListQuery>,
) -> Response {
    if session::verify(&token.0, state.credential().key()).is_none() {
        return errors::error(StatusCode::UNAUTHORIZED, "Not authenticated");
    }

    let q = filter_q(query.filter.as_deref());
    if q.is_empty() {
        return Json(Vec::<Song>::new()).into_response();
    }

    let search = match (
        SearchQuery::new(q),
        SearchLimit::new(result_limit(query.range.as_deref())),
    ) {
        (Ok(query), Ok(limit)) => CatalogueSearch::new(query, limit),
        _ => {
            return errors::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid search parameters.",
            );
        }
    };

    let tracks = match state.catalogue().search(&search, &context).await {
        Ok(tracks) => tracks,
        Err(error) => {
            tracing::error!(error = %error, request_id = context.request_id().as_str(), "catalogue search failed");
            return errors::error(StatusCode::INTERNAL_SERVER_ERROR, "Internal error.");
        }
    };

    Json(tracks.into_iter().map(Song::from).collect::<Vec<_>>()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{TrackId, TrackMetadata};

    fn song() -> Song {
        Track::new(
            TrackId::new(),
            TrackMetadata::new(
                "title".to_owned(),
                vec!["a".to_owned(), "b".to_owned()],
                Some(180_000),
            )
            .expect("valid metadata"),
        )
        .into()
    }

    #[test]
    fn track_maps_to_song() {
        let song = song();
        assert_eq!(song.title, "title");
        assert_eq!(song.artist, "a, b");
        assert_eq!(song.duration, 180.0);
        assert!(!song.id.is_empty());
    }

    #[test]
    fn filter_q_reads_the_query_value() {
        assert_eq!(filter_q(None), "");
        assert_eq!(filter_q(Some("garbage")), "");
        assert_eq!(filter_q(Some(r#"{"q":"daft punk"}"#)), "daft punk");
        assert_eq!(filter_q(Some(r#"{"q":"a\"b"}"#)), r#"a"b"#);
        assert_eq!(filter_q(Some(r#"{"other":1}"#)), "");
        assert_eq!(filter_q(Some(r#"{"q":42}"#)), "");
    }

    #[test]
    fn result_limit_follows_the_range() {
        assert_eq!(result_limit(None), 25);
        assert_eq!(result_limit(Some("[0,24]")), 25);
        assert_eq!(result_limit(Some("[0,9]")), 10);
        assert_eq!(result_limit(Some("[0,999]")), 25);
        assert_eq!(result_limit(Some("garbage")), 25);
    }
}
