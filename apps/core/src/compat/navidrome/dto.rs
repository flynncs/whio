use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub(crate) struct LoginBody {
    pub(crate) username: String,
    pub(crate) password: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthPayload {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) username: String,
    pub(crate) is_admin: bool,
    pub(crate) token: String,
    pub(crate) subsonic_salt: String,
    pub(crate) subsonic_token: String,
}

#[derive(Deserialize)]
pub(crate) struct SongListQuery {
    pub(crate) title: Option<String>,
    pub(crate) album_id: Option<String>,
    #[serde(rename = "_start")]
    pub(crate) start: Option<u32>,
    #[serde(rename = "_end")]
    pub(crate) end: Option<u32>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Song {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) album_id: Option<String>,
    pub(crate) album_artist: Option<String>,
    pub(crate) track_number: Option<i32>,
    pub(crate) disc_number: Option<i32>,
    pub(crate) cover_art: Option<String>,
    pub(crate) duration: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Album {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) album_artist: String,
    pub(crate) album_artist_id: String,
    pub(crate) song_count: u32,
    pub(crate) duration: f32,
    pub(crate) size: i64,
    pub(crate) year: Option<i32>,
    pub(crate) min_year: i32,
    pub(crate) max_year: i32,
    pub(crate) compilation: bool,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) genres: Option<Vec<Genre>>,
    pub(crate) library_id: i32,
    pub(crate) library_name: String,
    pub(crate) library_path: String,
    pub(crate) order_album_name: String,
    pub(crate) order_album_artist_name: String,
    pub(crate) external_info_updated_at: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Genre {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Artist {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) album_count: u32,
    pub(crate) song_count: u32,
    pub(crate) size: i64,
    pub(crate) missing: bool,
    pub(crate) sort_artist_name: String,
    pub(crate) order_artist_name: String,
    pub(crate) mbz_artist_id: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct NdError {
    pub(crate) error: String,
}
