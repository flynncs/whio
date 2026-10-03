use async_trait::async_trait;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::{
    delivery::{
        DeliveryError, DeliveryFailure, DeliveryRow, DeliveryState, DeliveryStatus, ListenPayload,
        ListenSource,
    },
    domain::TrackId,
    storage::contracts::{DeliveryStore, LibraryRead, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

type DeliveryRowColumns = (
    Uuid,
    Uuid,
    String,
    Uuid,
    chrono::DateTime<chrono::Utc>,
    String,
    Uuid,
    String,
    String,
    i32,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
    chrono::DateTime<chrono::Utc>,
);

type DeliveryStatusRow = (
    Uuid,
    String,
    String,
    i32,
    String,
    Option<String>,
    chrono::DateTime<chrono::Utc>,
);

const DELIVERY_COLUMNS: &str = "id, user_id, origin, track_id, listen_at, session_key, \
     destination_id, provider_id, state, attempts, provider_idempotence, provider_reference, \
     last_error, last_error_class, created_at, updated_at";

fn decode_row(row: DeliveryRowColumns) -> Result<DeliveryRow, TrackRepositoryError> {
    let state = DeliveryState::parse(&row.8).ok_or(TrackRepositoryError::InvalidData)?;

    Ok(DeliveryRow {
        id: row.0,
        user_id: row.1,
        origin: row.2,
        track_id: row.3,
        listen_at: row.4,
        session_key: row.5,
        destination_id: row.6,
        provider_id: row.7,
        state,
        attempts: row.9,
        provider_idempotence: row.10,
        provider_reference: row.11,
        last_error: row.12,
        last_error_class: row.13,
        created_at: row.14,
        updated_at: row.15,
    })
}

fn decode_status(row: DeliveryStatusRow) -> Result<DeliveryStatus, TrackRepositoryError> {
    let state = DeliveryState::parse(&row.2).ok_or(TrackRepositoryError::InvalidData)?;

    Ok(DeliveryStatus {
        destination_id: row.0,
        provider_id: row.1,
        state,
        attempts: row.3,
        provider_idempotence: row.4,
        last_error: row.5,
        updated_at: row.6,
    })
}

#[async_trait]
impl DeliveryStore for PgStore {
    async fn insert_pending(
        &self,
        tx: &mut PgConnection,
        row: &DeliveryRow,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            "INSERT INTO listen_deliveries \
             (id, user_id, origin, track_id, listen_at, session_key, destination_id, provider_id, \
              state, attempts, provider_idempotence, provider_reference, last_error, \
              last_error_class, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        )
        .bind(row.id)
        .bind(row.user_id)
        .bind(&row.origin)
        .bind(row.track_id)
        .bind(row.listen_at)
        .bind(&row.session_key)
        .bind(row.destination_id)
        .bind(&row.provider_id)
        .bind(row.state.as_str())
        .bind(row.attempts)
        .bind(&row.provider_idempotence)
        .bind(row.provider_reference.as_deref())
        .bind(row.last_error.as_deref())
        .bind(row.last_error_class.as_deref())
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(repository_error)?;

        Ok(())
    }

    async fn find(&self, delivery_id: Uuid) -> Result<Option<DeliveryRow>, TrackRepositoryError> {
        let row = sqlx::query_as::<_, DeliveryRowColumns>(&format!(
            "SELECT {DELIVERY_COLUMNS} FROM listen_deliveries WHERE id = $1"
        ))
        .bind(delivery_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)?;

        row.map(decode_row).transpose()
    }

    async fn transition(
        &self,
        delivery_id: Uuid,
        from: DeliveryState,
        to: DeliveryState,
        attempts: i32,
        provider_reference: Option<&str>,
        failure: Option<&DeliveryFailure>,
    ) -> Result<bool, TrackRepositoryError> {
        let updated = sqlx::query(
            "UPDATE listen_deliveries \
             SET state = $3, attempts = $4, provider_reference = COALESCE($5, provider_reference), \
                 last_error = $6, last_error_class = $7, updated_at = CURRENT_TIMESTAMP \
             WHERE id = $1 AND state = $2",
        )
        .bind(delivery_id)
        .bind(from.as_str())
        .bind(to.as_str())
        .bind(attempts)
        .bind(provider_reference)
        .bind(failure.map(|failure| failure.message.as_str()))
        .bind(failure.map(|failure| failure.class))
        .execute(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(updated.rows_affected() == 1)
    }

    async fn list_for_user(
        &self,
        user_id: Uuid,
        state: Option<DeliveryState>,
        limit: u32,
    ) -> Result<Vec<DeliveryStatus>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, DeliveryStatusRow>(
            "SELECT destination_id, provider_id, state, attempts, provider_idempotence, \
                    last_error, updated_at \
             FROM listen_deliveries \
             WHERE user_id = $1 AND ($2::TEXT IS NULL OR state = $2) \
             ORDER BY updated_at DESC, id DESC LIMIT $3",
        )
        .bind(user_id)
        .bind(state.map(DeliveryState::as_str))
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_status).collect()
    }
}

#[async_trait]
impl ListenSource for PgStore {
    async fn listen_payload(&self, track_id: Uuid) -> Result<Option<ListenPayload>, DeliveryError> {
        let track_id = TrackId::from_uuid(track_id);
        let Some(track) = LibraryRead::find_track(self, &track_id)
            .await
            .map_err(DeliveryError::Repository)?
        else {
            return Ok(None);
        };
        let album = LibraryRead::find_album_for_track(self, &track_id)
            .await
            .map_err(DeliveryError::Repository)?;

        ListenPayload::from_track(&track, album.as_ref()).map(Some)
    }
}
