use sqlx::PgPool;

mod catalogue;
pub(crate) mod decode;
mod library;
mod reports;
pub(crate) mod schema;
mod sources;
mod state;

pub struct PostgresTrackRepository {
    pool: PgPool,
}

pub(crate) type PgStore = PostgresTrackRepository;

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}
