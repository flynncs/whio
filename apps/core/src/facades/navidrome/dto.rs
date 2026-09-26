use serde::{Deserialize, Serialize};

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

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
    pub(crate) end: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Song {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) album: Option<String>,
    pub(crate) album_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) track_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) disc_number: Option<i32>,
    pub(crate) cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) duration: Option<f32>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) starred_at: Option<String>,
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub(crate) rating: u8,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub(crate) play_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) play_date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Album {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) album_artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) album_artist_id: Option<String>,
    pub(crate) song_count: u32,
    pub(crate) duration: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) year: Option<i32>,
    pub(crate) created_at: String,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) starred_at: Option<String>,
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub(crate) rating: u8,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub(crate) play_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) play_date: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Genre {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub(crate) song_count: u64,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub(crate) album_count: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Artist {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) album_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) song_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cover_art: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sort_artist_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mbz_artist_id: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) starred: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) starred_at: Option<String>,
    #[serde(skip_serializing_if = "is_zero_u8")]
    pub(crate) rating: u8,
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub(crate) play_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) play_date: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Playlist {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) comment: Option<String>,
    pub(crate) owner: String,
    pub(crate) public: bool,
    pub(crate) song_count: u32,
    pub(crate) duration: f32,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) songs: Vec<Song>,
}

#[derive(Serialize)]
pub(crate) struct NdError {
    pub(crate) error: String,
}
