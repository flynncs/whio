use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("database connection failed")]
    Connect(#[source] sqlx::Error),

    #[error("database migration failed")]
    Migrate(#[source] sqlx::migrate::MigrateError),

    #[error("database readiness check failed")]
    Check(#[source] sqlx::Error),
}

pub struct Database {
    pool: PgPool,
}

impl Database {
    pub async fn connect(url: &str) -> Result<Self, DatabaseError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(DatabaseError::Connect)?;

        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(DatabaseError::Migrate)?;

        Ok(Self { pool })
    }

    pub(crate) async fn check(&self) -> Result<(), DatabaseError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(DatabaseError::Check)
    }
}
