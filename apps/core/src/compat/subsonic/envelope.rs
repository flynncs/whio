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

pub(crate) fn search3_envelope(result: super::service::SearchResult) -> models::Search3Response {
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
                        artist: (!result.artists.is_empty()).then_some(result.artists),
                        album: (!result.albums.is_empty()).then_some(result.albums),
                        song: (!result.songs.is_empty()).then_some(result.songs),
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn album_list2_envelope(albums: Vec<models::AlbumId3>) -> models::GetAlbumList2Response {
    models::GetAlbumList2Response {
        subsonic_response: Some(Box::new(
            models::GetAlbumList2ResponseSubsonicResponse::GetAlbumList2SuccessResponse(Box::new(
                models::GetAlbumList2SuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_album_list2_success_response::Status::Ok,
                    album_list2: Box::new(models::AlbumList2 {
                        album: Some(albums),
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn album_envelope(album: models::AlbumId3WithSongs) -> models::GetAlbumResponse {
    models::GetAlbumResponse {
        subsonic_response: Some(Box::new(
            models::GetAlbumResponseSubsonicResponse::GetAlbumSuccessResponse(Box::new(
                models::GetAlbumSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_album_success_response::Status::Ok,
                    album: Box::new(album),
                },
            )),
        )),
    }
}

pub(crate) fn artists_envelope(indexes: Vec<models::IndexId3>) -> models::GetArtistsResponse {
    models::GetArtistsResponse {
        subsonic_response: Some(Box::new(
            models::GetArtistsResponseSubsonicResponse::GetArtistsSuccessResponse(Box::new(
                models::GetArtistsSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_artists_success_response::Status::Ok,
                    artists: Box::new(models::ArtistsId3 {
                        index: Some(indexes),
                        ..Default::default()
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn artist_envelope(artist: models::ArtistWithAlbumsId3) -> models::GetArtistResponse {
    models::GetArtistResponse {
        subsonic_response: Some(Box::new(
            models::GetArtistResponseSubsonicResponse::GetArtistSuccessResponse(Box::new(
                models::GetArtistSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_artist_success_response::Status::Ok,
                    artist: Box::new(artist),
                },
            )),
        )),
    }
}
