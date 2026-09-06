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
    pub(crate) duration: f32,
}

#[derive(Serialize)]
pub(crate) struct NdError {
    pub(crate) error: String,
}
