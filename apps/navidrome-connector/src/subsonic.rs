use crate::errors::ConnectorError;

pub(crate) fn normalize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim_end_matches('/');
    let (scheme, rest) = trimmed.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if scheme == "http"
        && !rest
            .split(['/', ':'])
            .next()
            .is_some_and(|host| host == "localhost" || host == "127.0.0.1" || host == "::1")
    {
        return None;
    }
    Some(trimmed.to_owned())
}

#[derive(Debug, serde::Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: ResponseBody,
}

#[derive(Debug, serde::Deserialize)]
struct ResponseBody {
    status: String,
    #[serde(default)]
    error: Option<ApiError>,
    #[serde(default)]
    users: Option<UserList>,
    #[serde(rename = "searchResult3", default)]
    search: Option<SearchResult>,
}

#[derive(Debug, serde::Deserialize)]
struct ApiError {
    code: u32,
}

#[derive(Debug, serde::Deserialize)]
struct UserList {
    user: OneOrMany<User>,
}

#[derive(Debug, serde::Deserialize)]
struct User {
    username: String,
}

#[derive(Debug, serde::Deserialize)]
struct SearchResult {
    #[serde(default)]
    song: OneOrMany<Song>,
}

#[derive(Debug, serde::Deserialize)]
struct Song {
    id: String,
    title: String,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    artists: Option<Vec<Artist>>,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    album: Option<String>,
    #[serde(rename = "coverArt", default)]
    cover_art: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct Artist {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(item) => vec![item],
            Self::Many(items) => items,
        }
    }
}

fn auth_params(username: &str, password: &str) -> Vec<(String, String)> {
    use md5::{Digest, Md5};

    let salt: String = uuid::Uuid::now_v7()
        .simple()
        .to_string()
        .chars()
        .take(16)
        .collect();
    let mut hasher = Md5::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let token = hex::encode(hasher.finalize());
    vec![
        ("u".to_owned(), username.to_owned()),
        ("t".to_owned(), token),
        ("s".to_owned(), salt),
        ("v".to_owned(), "1.16.1".to_owned()),
        ("c".to_owned(), "whio-nd-connector".to_owned()),
        ("f".to_owned(), "json".to_owned()),
    ]
}

fn parse_envelope(body: &str) -> Result<ResponseBody, ConnectorError> {
    let envelope: Envelope =
        serde_json::from_str(body).map_err(|_| ConnectorError::UpstreamBadResponse)?;
    if envelope.response.status != "ok" {
        let code = envelope.response.error.map(|error| error.code).unwrap_or(0);
        return Err(match code {
            40 => ConnectorError::UpstreamAuth,
            50 => ConnectorError::UpstreamForbidden,
            70 => ConnectorError::SourceNotFound,
            _ => ConnectorError::UpstreamBadResponse,
        });
    }
    Ok(envelope.response)
}

fn parse_usernames(body: &str) -> Result<Vec<String>, ConnectorError> {
    let response = parse_envelope(body)?;
    Ok(response
        .users
        .map(|users| users.user.into_vec())
        .unwrap_or_default()
        .into_iter()
        .map(|user| user.username)
        .filter(|username| !username.is_empty())
        .collect())
}

pub(crate) async fn fetch_usernames(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<Vec<String>, ConnectorError> {
    let Some(base_url) = normalize_base_url(base_url) else {
        return Err(ConnectorError::InvalidRequest);
    };

    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(auth_params(username, password))
        .finish();
    let body = client
        .get(format!("{base_url}/rest/getUsers.view?{query}"))
        .send()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .error_for_status()
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .text()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?;

    parse_usernames(&body)
}

pub(crate) async fn verify_credentials(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(), ConnectorError> {
    let Some(base_url) = normalize_base_url(base_url) else {
        return Err(ConnectorError::InvalidRequest);
    };

    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(auth_params(username, password))
        .finish();
    let body = client
        .get(format!("{base_url}/rest/ping.view?{query}"))
        .send()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .error_for_status()
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .text()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?;

    parse_envelope(&body).map(|_| ())
}

pub(crate) async fn resolve_stream_url(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
    external_id: &str,
) -> Result<String, ConnectorError> {
    let url = media_url(base_url, username, password, "stream.view", external_id)?;
    probe_media_url(client, url).await
}

pub(crate) async fn resolve_artwork_url(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
    artwork_id: &str,
) -> Result<String, ConnectorError> {
    let url = media_url(base_url, username, password, "getCoverArt.view", artwork_id)?;
    probe_media_url(client, url).await
}

fn media_url(
    base_url: &str,
    username: &str,
    password: &str,
    view: &str,
    external_id: &str,
) -> Result<String, ConnectorError> {
    let Some(base_url) = normalize_base_url(base_url) else {
        return Err(ConnectorError::InvalidRequest);
    };

    let mut params = auth_params(username, password);
    params.push(("id".to_owned(), external_id.to_owned()));
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish();
    Ok(format!("{base_url}/rest/{view}?{query}"))
}

/// token auth lives in the query, so the caller can fetch the URL directly. the
/// connector only proves the URL answers before handing it over.
async fn probe_media_url(client: &reqwest::Client, url: String) -> Result<String, ConnectorError> {
    let probe = client
        .get(&url)
        .header("Range", "bytes=0-0")
        .send()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?;
    match probe.status() {
        reqwest::StatusCode::UNAUTHORIZED => return Err(ConnectorError::UpstreamAuth),
        reqwest::StatusCode::NOT_FOUND => return Err(ConnectorError::SourceNotFound),
        status if !status.is_success() => return Err(ConnectorError::UpstreamBadResponse),
        _ => {}
    }

    let is_envelope = probe
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("xml") || content_type.contains("json"));
    if is_envelope {
        let body = probe
            .text()
            .await
            .map_err(|_| ConnectorError::ProviderUnavailable)?;
        parse_envelope(&body).map(|_| ())?;
    }

    Ok(url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamSong {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) duration_ms: Option<u64>,
    pub(crate) album: Option<String>,
    pub(crate) artwork_id: Option<String>,
}

fn song_artist(song: &Song) -> Option<String> {
    if let Some(artist) = song.artist.as_deref().filter(|artist| !artist.is_empty()) {
        return Some(artist.to_owned());
    }
    let artists: Vec<&str> = song
        .artists
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|artist| artist.name.as_str())
        .filter(|name| !name.is_empty())
        .collect();
    (!artists.is_empty()).then(|| artists.join(", "))
}

pub(crate) struct SongPage {
    pub(crate) songs: Vec<UpstreamSong>,
    pub(crate) next_offset: Option<u32>,
}

fn parse_songs(body: &str, offset: u32, limit: u32) -> Result<SongPage, ConnectorError> {
    let response = parse_envelope(body)?;
    let rows = response
        .search
        .map(|search| search.song.into_vec())
        .unwrap_or_default();
    let next = offset + limit;
    let next_offset = (rows.len() >= limit as usize && next <= 1000).then_some(next);
    let mut songs = Vec::new();
    let mut skipped = 0;
    for song in rows.into_iter().take(limit as usize) {
        let Some(artist) = song_artist(&song) else {
            skipped += 1;
            continue;
        };
        if song.id.is_empty() || song.title.is_empty() {
            skipped += 1;
            continue;
        }
        songs.push(UpstreamSong {
            id: song.id,
            title: song.title,
            artist,
            duration_ms: song.duration.map(|seconds| seconds.saturating_mul(1000)),
            album: song.album.filter(|album| !album.trim().is_empty()),
            artwork_id: song.cover_art.filter(|id| !id.trim().is_empty()),
        });
    }
    if skipped > 0 {
        tracing::debug!(skipped, "upstream songs lacked display metadata");
    }
    Ok(SongPage { songs, next_offset })
}

pub(crate) async fn search_songs(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
    query: &str,
    limit: u32,
    offset: u32,
) -> Result<SongPage, ConnectorError> {
    let Some(base_url) = normalize_base_url(base_url) else {
        return Err(ConnectorError::InvalidRequest);
    };

    let mut params = auth_params(username, password);
    params.push(("query".to_owned(), query.to_owned()));
    params.push(("songCount".to_owned(), limit.to_string()));
    params.push(("songOffset".to_owned(), offset.to_string()));
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish();
    let body = client
        .get(format!("{base_url}/rest/search3.view?{query}"))
        .send()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .error_for_status()
        .map_err(|_| ConnectorError::ProviderUnavailable)?
        .text()
        .await
        .map_err(|_| ConnectorError::ProviderUnavailable)?;

    parse_songs(&body, offset, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_rows_map_and_incomplete_rows_skip() {
        let body = r#"{"subsonic-response": {"status": "ok", "version": "1.16.1", "searchResult3": {"song": [{"id": "abc", "title": "Fish & Chips", "artist": "A <B>", "album": "X", "duration": 237}, {"id": "nope", "title": "No Artist"}, {"id": "nodur", "title": "T", "artist": "A"}]}}}"#;
        assert_eq!(
            parse_songs(body, 0, 25).unwrap().songs,
            vec![
                UpstreamSong {
                    id: "abc".to_owned(),
                    title: "Fish & Chips".to_owned(),
                    artist: "A <B>".to_owned(),
                    duration_ms: Some(237_000),
                    album: Some("X".to_owned()),
                    artwork_id: None,
                },
                UpstreamSong {
                    id: "nodur".to_owned(),
                    title: "T".to_owned(),
                    artist: "A".to_owned(),
                    duration_ms: None,
                    album: None,
                    artwork_id: None,
                },
            ]
        );
    }

    #[test]
    fn single_song_object_and_artist_arrays_map() {
        let body = r#"{"subsonic-response": {"status": "ok", "searchResult3": {"song": {"id": "one", "title": "T", "artists": [{"name": "A"}, {"name": "B"}], "duration": 10}}}}"#;
        assert_eq!(
            parse_songs(body, 0, 25).unwrap().songs,
            vec![UpstreamSong {
                id: "one".to_owned(),
                title: "T".to_owned(),
                artist: "A, B".to_owned(),
                duration_ms: Some(10_000),
                album: None,
                artwork_id: None,
            }]
        );
    }

    #[test]
    fn skipped_rows_do_not_end_pagination() {
        let body = r#"{"subsonic-response":{"status":"ok","searchResult3":{"song":[{"id":"bad","title":"No artist"},{"id":"good","title":"Song","artist":"Artist"}]}}}"#;
        let page = parse_songs(body, 25, 2).unwrap();
        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.next_offset, Some(27));
        assert_eq!(parse_songs(body, 1000, 2).unwrap().next_offset, None);
        assert_eq!(parse_songs(body, 25, 3).unwrap().next_offset, None);
    }

    #[test]
    fn base_url_rules_match_the_cli() {
        assert_eq!(
            normalize_base_url("https://music.example.test/"),
            Some("https://music.example.test".to_owned())
        );
        assert_eq!(
            normalize_base_url("http://localhost:4533"),
            Some("http://localhost:4533".to_owned())
        );
        assert_eq!(normalize_base_url("http://music.example.test"), None);
        assert_eq!(normalize_base_url("music.example.test"), None);
    }

    #[test]
    fn upstream_outcomes_map_to_errors() {
        let ok =
            r#"{"subsonic-response": {"status": "ok", "users": {"user": {"username": "admin"}}}}"#;
        assert_eq!(parse_usernames(ok).unwrap(), vec!["admin".to_owned()]);

        let denied = r#"{"subsonic-response": {"status": "failed", "error": {"code": 40, "message": "wrong"}}}"#;
        assert!(matches!(
            parse_usernames(denied),
            Err(ConnectorError::UpstreamAuth)
        ));

        let forbidden = r#"{"subsonic-response": {"status": "failed", "error": {"code": 50, "message": "admin"}}}"#;
        assert!(matches!(
            parse_usernames(forbidden),
            Err(ConnectorError::UpstreamForbidden)
        ));

        assert!(matches!(
            parse_usernames("<html>nope</html>"),
            Err(ConnectorError::UpstreamBadResponse)
        ));
    }
}
