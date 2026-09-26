use std::path::PathBuf;

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::features::music_library::{
    domain::SourceScope,
    library::{Library, LibraryId},
    storage::contracts::{LibraryStore, TrackRepositoryError},
};

use super::decode::{encode_source_scope, repository_error};

pub struct PostgresLibraryStore {
    pool: PgPool,
}

type LibraryRow = (Uuid, i32, String);

impl PostgresLibraryStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn decode_library((id, numeric_id, label): LibraryRow) -> Library {
        Library::new(LibraryId::from_uuid(id), numeric_id, label)
    }

    async fn write_roots(
        transaction: &mut Transaction<'_, Postgres>,
        library_id: Uuid,
        roots: &[PathBuf],
    ) -> Result<(), TrackRepositoryError> {
        for root in roots {
            sqlx::query(
                r#"INSERT INTO library_roots (library_id, root) VALUES ($1, $2)
                ON CONFLICT (library_id, root) DO NOTHING
                "#,
            )
            .bind(library_id)
            .bind(root.display().to_string())
            .execute(&mut **transaction)
            .await
            .map_err(repository_error)?;
        }

        Ok(())
    }

    async fn find_row_by_label(
        &self,
        label: &str,
    ) -> Result<Option<LibraryRow>, TrackRepositoryError> {
        sqlx::query_as::<_, LibraryRow>(
            "SELECT id, numeric_id, label FROM libraries WHERE label = $1",
        )
        .bind(label)
        .fetch_optional(&self.pool)
        .await
        .map_err(repository_error)
    }
}

#[async_trait]
impl LibraryStore for PostgresLibraryStore {
    async fn find_by_label(&self, label: &str) -> Result<Option<Library>, TrackRepositoryError> {
        Ok(self
            .find_row_by_label(label)
            .await?
            .map(Self::decode_library))
    }

    async fn insert_library(
        &self,
        label: &str,
        roots: &[PathBuf],
    ) -> Result<Library, TrackRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        let inserted: Option<LibraryRow> = sqlx::query_as::<_, LibraryRow>(
            r#"INSERT INTO libraries (id, label) VALUES ($1, $2)
            ON CONFLICT (label) DO NOTHING
            RETURNING id, numeric_id, label
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(label)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(repository_error)?;

        let row = match inserted {
            Some(row) => row,
            None => sqlx::query_as::<_, LibraryRow>(
                "SELECT id, numeric_id, label FROM libraries WHERE label = $1",
            )
            .bind(label)
            .fetch_one(&mut *transaction)
            .await
            .map_err(repository_error)?,
        };

        Self::write_roots(&mut transaction, row.0, roots).await?;
        transaction.commit().await.map_err(repository_error)?;

        Ok(Self::decode_library(row))
    }

    async fn replace_roots(
        &self,
        library_id: &LibraryId,
        roots: &[PathBuf],
    ) -> Result<(), TrackRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        sqlx::query("DELETE FROM library_roots WHERE library_id = $1")
            .bind(library_id.as_uuid())
            .execute(&mut *transaction)
            .await
            .map_err(repository_error)?;

        Self::write_roots(&mut transaction, *library_id.as_uuid(), roots).await?;
        transaction.commit().await.map_err(repository_error)?;

        Ok(())
    }

    async fn list_libraries(&self) -> Result<Vec<Library>, TrackRepositoryError> {
        let rows: Vec<LibraryRow> = sqlx::query_as::<_, LibraryRow>(
            "SELECT id, numeric_id, label FROM libraries ORDER BY numeric_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(rows.into_iter().map(Self::decode_library).collect())
    }

    async fn scopes_for_ids(&self, ids: &[LibraryId]) -> Result<Vec<String>, TrackRepositoryError> {
        let ids: Vec<Uuid> = ids.iter().map(|id| *id.as_uuid()).collect();

        let rows: Vec<(String,)> = sqlx::query_as::<_, (String,)>(
            r#"SELECT roots.root
            FROM library_roots AS roots
            JOIN libraries AS libraries ON libraries.id = roots.library_id
            WHERE roots.library_id = ANY($1)
            ORDER BY libraries.numeric_id, roots.root
            "#,
        )
        .bind(ids.as_slice())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(rows
            .into_iter()
            .map(|(root,)| encode_source_scope(&SourceScope::Named(root)))
            .collect())
    }
}
