use whio_subsonic_api::models;

pub(crate) const SUBSONIC_API_VERSION: &str = "1.16.1";
pub(crate) const INDEX_BASED_QUEUE_EXTENSION: &str = "indexBasedQueue";
pub(crate) const PLAYBACK_REPORT_EXTENSION: &str = "playbackReport";
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
                        open_subsonic_extensions: vec![
                            models::OpenSubsonicExtension {
                                name: INDEX_BASED_QUEUE_EXTENSION.to_owned(),
                                versions: vec![1],
                            },
                            models::OpenSubsonicExtension {
                                name: PLAYBACK_REPORT_EXTENSION.to_owned(),
                                versions: vec![1],
                            },
                        ],
                    },
                )),
        )),
    }
}

pub(crate) fn now_playing_envelope(
    entries: Vec<models::NowPlayingEntry>,
) -> models::GetNowPlayingResponse {
    models::GetNowPlayingResponse {
        subsonic_response: Some(Box::new(
            models::GetNowPlayingResponseSubsonicResponse::GetNowPlayingSuccessResponse(Box::new(
                models::GetNowPlayingSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_now_playing_success_response::Status::Ok,
                    now_playing: Box::new(models::NowPlaying { entry: entries }),
                },
            )),
        )),
    }
}

pub(crate) fn user_envelope(
    user: &crate::features::identity::service::DirectoryUser,
) -> models::GetUserResponse {
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
                        username: user.username.clone(),
                        scrobbling_enabled: false,
                        max_bit_rate: None,
                        admin_role: user.role.is_admin(),
                        settings_role: false,
                        download_role: false,
                        upload_role: false,
                        playlist_role: true,
                        cover_art_role: true,
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

pub(crate) fn song_envelope(song: models::Child) -> models::GetSongResponse {
    models::GetSongResponse {
        subsonic_response: Some(Box::new(
            models::GetSongResponseSubsonicResponse::GetSongSuccessResponse(Box::new(
                models::GetSongSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_song_success_response::Status::Ok,
                    song: Box::new(song),
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

pub(crate) fn artist_info_envelope(info: models::ArtistInfo) -> models::GetArtistInfoResponse {
    models::GetArtistInfoResponse {
        subsonic_response: Some(Box::new(
            models::GetArtistInfoResponseSubsonicResponse::GetArtistInfoSuccessResponse(Box::new(
                models::GetArtistInfoSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_artist_info_success_response::Status::Ok,
                    artist_info: Box::new(info),
                },
            )),
        )),
    }
}

pub(crate) fn artist_info2_envelope(info: models::ArtistInfo2) -> models::GetArtistInfo2Response {
    models::GetArtistInfo2Response {
        subsonic_response: Some(Box::new(
            models::GetArtistInfo2ResponseSubsonicResponse::GetArtistInfo2SuccessResponse(
                Box::new(models::GetArtistInfo2SuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_artist_info2_success_response::Status::Ok,
                    artist_info2: Box::new(info),
                }),
            ),
        )),
    }
}

pub(crate) fn similar_songs_envelope(
    songs: models::SimilarSongs,
) -> models::GetSimilarSongsResponse {
    models::GetSimilarSongsResponse {
        subsonic_response: Some(Box::new(
            models::GetSimilarSongsResponseSubsonicResponse::GetSimilarSongsSuccessResponse(
                Box::new(models::GetSimilarSongsSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_similar_songs_success_response::Status::Ok,
                    similar_songs: Box::new(songs),
                }),
            ),
        )),
    }
}

pub(crate) fn similar_songs2_envelope(
    songs: models::SimilarSongs2,
) -> models::GetSimilarSongs2Response {
    models::GetSimilarSongs2Response {
        subsonic_response: Some(Box::new(
            models::GetSimilarSongs2ResponseSubsonicResponse::GetSimilarSongs2SuccessResponse(
                Box::new(models::GetSimilarSongs2SuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_similar_songs2_success_response::Status::Ok,
                    similar_songs2: Box::new(songs),
                }),
            ),
        )),
    }
}

pub(crate) fn top_songs_envelope(songs: models::TopSongs) -> models::GetTopSongsResponse {
    models::GetTopSongsResponse {
        subsonic_response: Some(Box::new(
            models::GetTopSongsResponseSubsonicResponse::GetTopSongsSuccessResponse(Box::new(
                models::GetTopSongsSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_top_songs_success_response::Status::Ok,
                    top_songs: Box::new(songs),
                },
            )),
        )),
    }
}

pub(crate) fn scan_status_envelope(
    status: &crate::features::music_library::library::ScanStatus,
) -> models::GetScanStatusResponse {
    models::GetScanStatusResponse {
        subsonic_response: Some(Box::new(
            models::GetScanStatusResponseSubsonicResponse::GetScanStatusSuccessResponse(Box::new(
                models::GetScanStatusSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_scan_status_success_response::Status::Ok,
                    scan_status: Box::new(scan_status(status)),
                },
            )),
        )),
    }
}

pub(crate) fn start_scan_envelope(
    status: &crate::features::music_library::library::ScanStatus,
) -> models::StartScanResponse {
    models::StartScanResponse {
        subsonic_response: Some(Box::new(
            models::StartScanResponseSubsonicResponse::StartScanSuccessResponse(Box::new(
                models::StartScanSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::start_scan_success_response::Status::Ok,
                    scan_status: Box::new(scan_status(status)),
                },
            )),
        )),
    }
}

fn scan_status(status: &crate::features::music_library::library::ScanStatus) -> models::ScanStatus {
    models::ScanStatus {
        scanning: status.scanning,
        count: Some(status.count as i32),
    }
}

pub(crate) fn starred2_envelope(
    result: super::service::StarredResult,
) -> models::GetStarred2Response {
    models::GetStarred2Response {
        subsonic_response: Some(Box::new(
            models::GetStarred2ResponseSubsonicResponse::GetStarred2SuccessResponse(Box::new(
                models::GetStarred2SuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_starred2_success_response::Status::Ok,
                    starred2: Box::new(models::Starred2 {
                        artist: (!result.artists.is_empty()).then_some(result.artists),
                        album: (!result.albums.is_empty()).then_some(result.albums),
                        song: (!result.songs.is_empty()).then_some(result.songs),
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn playlists_envelope(playlists: Vec<models::Playlist>) -> models::GetPlaylistsResponse {
    models::GetPlaylistsResponse {
        subsonic_response: Some(Box::new(
            models::GetPlaylistsResponseSubsonicResponse::GetPlaylistsSuccessResponse(Box::new(
                models::GetPlaylistsSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_playlists_success_response::Status::Ok,
                    playlists: Box::new(models::Playlists {
                        playlist: (!playlists.is_empty()).then_some(playlists),
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn playlist_envelope(
    playlist: models::PlaylistWithSongs,
) -> models::GetPlaylistResponse {
    models::GetPlaylistResponse {
        subsonic_response: Some(Box::new(
            models::GetPlaylistResponseSubsonicResponse::GetPlaylistSuccessResponse(Box::new(
                models::GetPlaylistSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_playlist_success_response::Status::Ok,
                    playlist: Box::new(playlist),
                },
            )),
        )),
    }
}

pub(crate) fn create_playlist_envelope(
    playlist: models::PlaylistWithSongs,
) -> models::CreatePlaylistResponse {
    models::CreatePlaylistResponse {
        subsonic_response: Some(Box::new(
            models::CreatePlaylistResponseSubsonicResponse::CreatePlaylistSuccessResponse(
                Box::new(models::CreatePlaylistSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::create_playlist_success_response::Status::Ok,
                    playlist: Box::new(playlist),
                }),
            ),
        )),
    }
}

pub(crate) fn genres_envelope(genres: Vec<models::Genre>) -> models::GetGenresResponse {
    models::GetGenresResponse {
        subsonic_response: Some(Box::new(
            models::GetGenresResponseSubsonicResponse::GetGenresSuccessResponse(Box::new(
                models::GetGenresSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_genres_success_response::Status::Ok,
                    genres: Box::new(models::Genres {
                        genre: (!genres.is_empty()).then_some(genres),
                    }),
                },
            )),
        )),
    }
}

pub(crate) fn songs_by_genre_envelope(
    songs: Vec<models::Child>,
) -> models::GetSongsByGenreResponse {
    models::GetSongsByGenreResponse {
        subsonic_response: Some(Box::new(
            models::GetSongsByGenreResponseSubsonicResponse::GetSongsByGenreSuccessResponse(
                Box::new(models::GetSongsByGenreSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_songs_by_genre_success_response::Status::Ok,
                    songs_by_genre: Box::new(models::Songs {
                        song: (!songs.is_empty()).then_some(songs),
                    }),
                }),
            ),
        )),
    }
}

pub(crate) fn random_songs_envelope(songs: Vec<models::Child>) -> models::GetRandomSongsResponse {
    models::GetRandomSongsResponse {
        subsonic_response: Some(Box::new(
            models::GetRandomSongsResponseSubsonicResponse::GetRandomSongsSuccessResponse(
                Box::new(models::GetRandomSongsSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_random_songs_success_response::Status::Ok,
                    random_songs: Box::new(models::Songs {
                        song: (!songs.is_empty()).then_some(songs),
                    }),
                }),
            ),
        )),
    }
}

pub(crate) fn indexes_envelope(indexes: models::Indexes) -> models::GetIndexesResponse {
    models::GetIndexesResponse {
        subsonic_response: Some(Box::new(
            models::GetIndexesResponseSubsonicResponse::GetIndexesSuccessResponse(Box::new(
                models::GetIndexesSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_indexes_success_response::Status::Ok,
                    indexes: Box::new(indexes),
                },
            )),
        )),
    }
}

pub(crate) fn directory_envelope(
    directory: models::Directory,
) -> models::GetMusicDirectoryResponse {
    models::GetMusicDirectoryResponse {
        subsonic_response: Some(Box::new(
            models::GetMusicDirectoryResponseSubsonicResponse::GetMusicDirectorySuccessResponse(
                Box::new(models::GetMusicDirectorySuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_music_directory_success_response::Status::Ok,
                    directory: Box::new(directory),
                }),
            ),
        )),
    }
}
pub(crate) fn play_queue_envelope(play_queue: models::PlayQueue) -> models::GetPlayQueueResponse {
    models::GetPlayQueueResponse {
        subsonic_response: Some(Box::new(
            models::GetPlayQueueResponseSubsonicResponse::GetPlayQueueSuccessResponse(Box::new(
                models::GetPlayQueueSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_play_queue_success_response::Status::Ok,
                    play_queue: Box::new(play_queue),
                },
            )),
        )),
    }
}

pub(crate) fn play_queue_by_index_envelope(
    play_queue: models::PlayQueueByIndex,
) -> models::GetPlayQueueByIndexResponse {
    models::GetPlayQueueByIndexResponse {
        subsonic_response: Some(Box::new(
            models::GetPlayQueueByIndexResponseSubsonicResponse::GetPlayQueueByIndexSuccessResponse(
                Box::new(models::GetPlayQueueByIndexSuccessResponse {
                    version: SUBSONIC_API_VERSION.to_owned(),
                    r#type: SERVER_TYPE.to_owned(),
                    server_version: env!("CARGO_PKG_VERSION").to_owned(),
                    open_subsonic: true,
                    status: models::get_play_queue_by_index_success_response::Status::Ok,
                    play_queue_by_index: Box::new(play_queue),
                }),
            ),
        )),
    }
}
