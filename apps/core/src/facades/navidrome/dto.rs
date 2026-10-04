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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) suffix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bit_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) bit_depth: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) channels: Option<u8>,
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
    #[serde(rename = "coverArtId", skip_serializing_if = "Option::is_none")]
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
    pub(crate) owner_id: String,
    pub(crate) owner_name: String,
    pub(crate) public: bool,
    pub(crate) song_count: u32,
    pub(crate) duration: f32,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) songs: Option<Vec<PlaylistSong>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistSong {
    #[serde(flatten)]
    pub(crate) song: Song,
    pub(crate) media_file_id: String,
    pub(crate) playlist_id: String,
}

#[derive(Serialize)]
pub(crate) struct PlaylistCreated {
    pub(crate) id: String,
}

#[derive(Serialize)]
pub(crate) struct PlaylistAdded {
    pub(crate) added: u32,
}

#[derive(Serialize)]
pub(crate) struct PlaylistRemoved {
    pub(crate) ids: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct PlaylistMoved {
    pub(crate) id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistCreateBody {
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) comment: Option<String>,
    #[serde(default)]
    pub(crate) owner_id: Option<String>,
    #[serde(default)]
    pub(crate) public: Option<bool>,
    #[serde(default)]
    pub(crate) rules: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) sync: Option<bool>,
    #[serde(default)]
    pub(crate) ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaylistUpdateBody {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) comment: Option<String>,
    #[serde(default)]
    pub(crate) owner_id: Option<String>,
    #[serde(default)]
    pub(crate) public: Option<bool>,
    #[serde(default)]
    pub(crate) rules: Option<serde_json::Value>,
    #[serde(default)]
    pub(crate) sync: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct PlaylistTracksBody {
    #[serde(default)]
    pub(crate) ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub(crate) struct PlaylistMoveBody {
    pub(crate) insert_before: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QueueSaveBody {
    #[serde(default)]
    pub(crate) current: Option<u32>,
    #[serde(default)]
    pub(crate) ids: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) position: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Queue {
    pub(crate) changed_by: String,
    pub(crate) current: u32,
    pub(crate) items: Vec<Song>,
    pub(crate) position: u64,
    pub(crate) updated_at: String,
}

impl Queue {
    pub(crate) fn empty() -> Self {
        Self {
            changed_by: String::new(),
            current: 0,
            items: Vec::new(),
            position: 0,
            updated_at: String::new(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct NdError {
    pub(crate) error: String,
}
