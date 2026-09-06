use whio_subsonic_api::models;

pub(crate) const SUBSONIC_API_VERSION: &str = "1.16.1";
const SERVER_TYPE: &str = "whio";

pub(crate) fn ok_envelope() -> models::SubsonicResponse {
    models::SubsonicResponse {
        subsonic_response: Some(Box::new(
            models::SubsonicResponseSubsonicResponse::SubsonicSuccessResponse(Box::new(
                models::SubsonicSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::subsonic_success_response::Status::Ok,
                },
            )),
        )),
    }
}

pub(crate) fn failed_envelope(
    code: models::error::Code,
    message: &str,
) -> models::SubsonicResponse {
    models::SubsonicResponse {
        subsonic_response: Some(Box::new(
            models::SubsonicResponseSubsonicResponse::SubsonicFailureResponse(Box::new(
                models::SubsonicFailureResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::subsonic_failure_response::Status::Failed,
                    error: Box::new(models::Error {
                        code,
                        message: Some(message.to_owned()),
                        help_url: None,
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn extensions_envelope() -> models::GetOpenSubsonicExtensionsResponse {
    models::GetOpenSubsonicExtensionsResponse {
        subsonic_response: Some(Box::new(
            models::GetOpenSubsonicExtensionsResponseSubsonicResponse::
                GetOpenSubsonicExtensionsSuccessResponse(Box::new(
                    models::GetOpenSubsonicExtensionsSuccessResponse {
                        version: SUBSONIC_API_VERSION.to_owned(),
                        r#type: SERVER_TYPE.to_owned(),
                        server_version: env!("CARGO_PKG_VERSION").to_owned(),
                        open_subsonic: true,
                        status: models::get_open_subsonic_extensions_success_response::Status::Ok,
                        open_subsonic_extensions: Vec::new(),
                    },
                )),
        )),
    }
}

pub(crate) fn user_envelope(username: &str) -> models::GetUserResponse {
    models::GetUserResponse {
        subsonic_response: Some(Box::new(
            models::GetUserResponseSubsonicResponse::GetUserSuccessResponse(Box::new(
                models::GetUserSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_user_success_response::Status::Ok,
                    user: Box::new(models::User {
                        username: username.to_owned(),
                        scrobbling_enabled: false,
                        max_bit_rate: None,
                        admin_role: false,
                        settings_role: false,
                        download_role: false,
                        upload_role: false,
                        playlist_role: false,
                        cover_art_role: false,
                        comment_role: false,
                        podcast_role: false,
                        stream_role: true,
                        jukebox_role: false,
                        share_role: false,
                        video_conversion_role: false,
                        avatar_last_changed: None,
                        folder: None,
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn search3_envelope(songs: Vec<models::Child>) -> models::Search3Response {
    models::Search3Response {
        subsonic_response: Some(Box::new(
            models::Search3ResponseSubsonicResponse::Search3SuccessResponse(Box::new(
                models::Search3SuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::search3_success_response::Status::Ok,
                    search_result3: Box::new(models::SearchResult3 {
                        song: Some(songs),
                        ..Default::default()
                    }),
                },
            )),
        )),
    }
}
