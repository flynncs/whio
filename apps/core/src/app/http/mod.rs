mod middleware;
pub(crate) mod routes;

use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    app::AppState,
    facades,
    features::identity,
    features::{music_library, playback},
};
use axum::middleware::from_fn;

struct UnconstructedValidator;

#[async_trait::async_trait]
impl identity::destinations::DestinationValidator for UnconstructedValidator {
    async fn validate(
        &self,
        _provider_id: &identity::destinations::DestinationProvider,
        _token: &str,
    ) -> Result<identity::destinations::ValidatedDestination, identity::destinations::ValidationError>
    {
        Err(identity::destinations::ValidationError::Unavailable)
    }
}

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(routes::health::live))
        .route("/health/ready", get(routes::health::ready))
        .route(
            "/catalogue/search",
            post(music_library::api::native::search),
        )
        .route(
            "/api/v1/library/tracks",
            get(music_library::api::native::library_tracks),
        )
        .route(
            "/api/v1/library/albums",
            get(music_library::api::native::library_albums),
        )
        .route(
            "/api/v1/library/artists",
            get(music_library::api::native::library_artists),
        )
        .route(
            "/api/v1/playlists",
            get(music_library::api::playlists::list).post(music_library::api::playlists::create),
        )
        .route(
            "/api/v1/playlists/{id}",
            get(music_library::api::playlists::detail)
                .patch(music_library::api::playlists::update)
                .delete(music_library::api::playlists::remove),
        )
        .route(
            "/api/v1/playlists/{id}/entries/{entry_id}/move",
            post(music_library::api::playlists::move_entry),
        )
        .route("/api/v1/listens", post(music_library::api::history::submit))
        .route("/api/v1/history", get(music_library::api::history::read))
        .route(
            "/api/v1/activity",
            get(music_library::api::history::activity).post(music_library::api::history::report),
        )
        .route(
            "/api/v1/queue",
            get(music_library::api::queue::read)
                .put(music_library::api::queue::save)
                .delete(music_library::api::queue::clear),
        )
        .route(
            "/playback/tracks/{track_id}/stream",
            get(playback::api::native::stream),
        )
        .route(
            "/playback/tracks/{track_id}/artwork",
            get(music_library::api::native::artwork),
        )
        .nest("/rest", facades::subsonic::router())
        .nest(
            "/compat/subsonic/rest",
            facades::subsonic::router().layer(from_fn(facades::cors)),
        )
        .nest(
            "/compat/navidrome/rest",
            facades::subsonic::router().layer(from_fn(facades::cors)),
        )
        .nest(
            "/compat/navidrome",
            facades::navidrome::router::<AppState>().layer(from_fn(facades::cors)),
        )
        .merge(music_library::api::admin::routes().with_state(
            music_library::api::admin::AdminState {
                playlists: std::sync::Arc::clone(&state.playlists),
                credential: std::sync::Arc::clone(&state.credential),
            },
        ))
        .merge(music_library::api::artist_info::routes().with_state(
            music_library::api::artist_info::ArtistInfoState::with_defaults(
                std::sync::Arc::clone(&state.library),
                std::sync::Arc::clone(&state.credential),
                state.metadata_profile().clone(),
                state.metadata_language().to_owned(),
                state.discovery_service(),
            ),
        ))
        .merge(
            music_library::delivery_routes().with_state(music_library::ListenDeliveryState {
                delivery: state.delivery(),
                credential: std::sync::Arc::clone(&state.credential),
            }),
        )
        .merge(
            identity::destinations_router().with_state(identity::DestinationState::new(
                state.destinations().unwrap_or_else(|| {
                    std::sync::Arc::new(identity::disabled_destinations(std::sync::Arc::new(
                        UnconstructedValidator,
                    )))
                }),
                std::sync::Arc::clone(&state.credential),
            )),
        )
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn_with_state(
            std::sync::Arc::clone(&state.client_ip_policy),
            middleware::request_id,
        ))
        .with_state(state)
}
