use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{ListenOrigin, ListenOutcome, ListenSubmission, TrackId},
    history::ListenService,
    storage::{
        contracts::{LibraryRead, ListenStore},
        postgres::PostgresTrackRepository,
    },
};

pub fn listen_service(pool: &PgPool) -> ListenService {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));

    ListenService::new(
        Arc::clone(&store) as Arc<dyn ListenStore>,
        store as Arc<dyn LibraryRead>,
    )
}

pub async fn seed_play(
    pool: &PgPool,
    principal: Uuid,
    track: &TrackId,
    listen_at: DateTime<Utc>,
) -> ListenOutcome {
    listen_service(pool)
        .submit_listen(&ListenSubmission::new(
            principal,
            ListenOrigin::SubsonicScrobble,
            track.clone(),
            listen_at,
            None,
            None,
        ))
        .await
        .expect("the ledger accepts the seeded play")
}
