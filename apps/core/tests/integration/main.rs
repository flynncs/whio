#[cfg(feature = "postgres-integration")]
mod support;

#[path = "support/dedup.rs"]
mod dedup_fixture;

mod app_wiring;
#[cfg(feature = "postgres-integration")]
mod artwork_http;
#[cfg(feature = "postgres-integration")]
mod catalogue_http;
#[cfg(feature = "postgres-integration")]
mod compat_http;
mod dedup_eval;
mod dedup_parity;
#[cfg(feature = "postgres-integration")]
mod dedup_search;
#[cfg(feature = "postgres-integration")]
mod disabled_provider_http;
#[cfg(feature = "postgres-integration")]
mod health;
#[cfg(feature = "postgres-integration")]
mod library_state_http;
#[cfg(feature = "postgres-integration")]
mod listen_delivery_http;
#[cfg(feature = "postgres-integration")]
mod local_playback;
#[cfg(feature = "postgres-integration")]
mod native_artist_info_http;
#[cfg(feature = "postgres-integration")]
mod native_destinations_http;
#[cfg(feature = "postgres-integration")]
mod native_history_http;
#[cfg(feature = "postgres-integration")]
mod native_library_http;
#[cfg(feature = "postgres-integration")]
mod native_playlists_http;
#[cfg(feature = "postgres-integration")]
mod native_queue_http;
#[cfg(feature = "postgres-integration")]
mod navidrome_album_artist_http;
#[cfg(feature = "postgres-integration")]
mod navidrome_playlist_http;
#[cfg(feature = "postgres-integration")]
mod navidrome_song_http;
#[cfg(feature = "postgres-integration")]
mod playback_http;
mod resolver;
#[cfg(feature = "postgres-integration")]
mod rest_http;
#[cfg(feature = "postgres-integration")]
mod subsonic_artist_info_http;
#[cfg(feature = "postgres-integration")]
mod subsonic_search_windows;
#[cfg(feature = "postgres-integration")]
mod w1_matrix_http;
#[cfg(feature = "postgres-integration")]
mod worker_binary;
#[cfg(feature = "postgres-integration")]
mod worker_job;
#[cfg(feature = "postgres-integration")]
mod worker_jobs;
