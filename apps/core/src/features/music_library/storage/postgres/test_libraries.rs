use std::{path::PathBuf, sync::Arc};

use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    library::{LibraryId, LibraryRegistration, LibraryRegistrationError, LibraryRegistry, scan},
    storage::contracts::{CatalogueWrite, LibraryStore, SourceLookup},
};

use super::PostgresLibraryStore;
use crate::features::music_library::storage::postgres::PostgresTrackRepository;

fn registry(pool: &PgPool) -> LibraryRegistry {
    LibraryRegistry::new(Arc::new(PostgresLibraryStore::new(pool.clone())))
}

fn registration(label: &str, roots: &[&str]) -> LibraryRegistration {
    LibraryRegistration::new(label.to_owned(), roots.iter().map(PathBuf::from).collect())
}

fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac")
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("whio-libraries-{label}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn library_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM libraries")
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn scanned_track_ids(pool: &PgPool, scope: &str) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT DISTINCT track_id FROM track_sources WHERE source_scope = $1")
        .bind(scope)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn registration_is_idempotent_and_keeps_ids(pool: PgPool) {
    let registry = registry(&pool);
    let registrations = [registration("music", &["/a", "/b"])];

    let first = registry.register(&registrations).await.unwrap();
    let second = registry.register(&registrations).await.unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(first, second);
    assert_eq!(library_count(&pool).await, 1);
}

#[sqlx::test]
async fn numeric_ids_survive_reorder_and_a_new_registration(pool: PgPool) {
    let registry = registry(&pool);

    let first = registry
        .register(&[
            registration("music", &["/a"]),
            registration("books", &["/b"]),
        ])
        .await
        .unwrap();
    let music = first
        .iter()
        .find(|library| library.label() == "music")
        .unwrap();
    let books = first
        .iter()
        .find(|library| library.label() == "books")
        .unwrap();
    assert_eq!(music.numeric_id(), 1);
    assert_eq!(books.numeric_id(), 2);

    let reordered = registry
        .register(&[
            registration("books", &["/b"]),
            registration("music", &["/a"]),
        ])
        .await
        .unwrap();
    let music_after = reordered
        .iter()
        .find(|library| library.label() == "music")
        .unwrap();
    let books_after = reordered
        .iter()
        .find(|library| library.label() == "books")
        .unwrap();
    assert_eq!(music_after.id(), music.id());
    assert_eq!(music_after.numeric_id(), music.numeric_id());
    assert_eq!(books_after.id(), books.id());
    assert_eq!(books_after.numeric_id(), books.numeric_id());

    let extended = registry
        .register(&[
            registration("music", &["/a"]),
            registration("books", &["/b"]),
            registration("audiobooks", &["/c"]),
        ])
        .await
        .unwrap();
    let audiobooks = extended
        .iter()
        .find(|library| library.label() == "audiobooks")
        .unwrap();
    assert_eq!(audiobooks.numeric_id(), books.numeric_id() + 1);
    assert_eq!(
        extended
            .iter()
            .find(|library| library.label() == "music")
            .unwrap()
            .numeric_id(),
        music.numeric_id()
    );
}

#[sqlx::test]
async fn no_projection_carries_a_path_and_l10_owns_the_wire_dto(pool: PgPool) {
    let registry = registry(&pool);
    let libraries = registry
        .register(&[registration("music", &["/tank/music", "/tank/music/live"])])
        .await
        .unwrap();

    let rendered = format!("{libraries:?}");
    assert!(rendered.contains("label: \"music\""), "{rendered}");
    assert!(rendered.contains("numeric_id: 1"), "{rendered}");
    assert!(!rendered.contains('/'), "{rendered}");
}

#[sqlx::test]
async fn scopes_for_returns_the_encoded_scopes(pool: PgPool) {
    let music_root = temp_root("scopes-music");
    std::fs::copy(fixture(), music_root.join("a.flac")).unwrap();
    let books_root = temp_root("scopes-books");
    std::fs::create_dir_all(books_root.join("nested")).unwrap();

    let registry = registry(&pool);
    let libraries = registry
        .register(&[
            registration("music", &[music_root.to_str().unwrap()]),
            registration(
                "books",
                &[
                    books_root.to_str().unwrap(),
                    books_root.join("nested").to_str().unwrap(),
                ],
            ),
        ])
        .await
        .unwrap();
    assert_eq!(libraries.len(), 2);

    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;
    let report = scan(&catalogue, &sources, std::slice::from_ref(&music_root))
        .await
        .unwrap();
    assert_eq!(report.indexed, 1);

    let recorded: String =
        sqlx::query_scalar("SELECT source_scope FROM track_sources WHERE provider_id = 'local'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(recorded, format!("named:{}", music_root.display()));

    assert_eq!(
        registry.scopes_for("music").await.unwrap(),
        vec![recorded.clone()]
    );
    assert_eq!(
        registry.scopes_for_numeric(1).await.unwrap(),
        vec![recorded.clone()]
    );

    let music = libraries
        .iter()
        .find(|library| library.label() == "music")
        .unwrap();
    let books = libraries
        .iter()
        .find(|library| library.label() == "books")
        .unwrap();

    assert_eq!(
        registry
            .scopes_for_ids(std::slice::from_ref(music.id()))
            .await
            .unwrap(),
        vec![recorded]
    );
    assert_eq!(
        registry
            .scopes_for_ids(&[music.id().clone(), books.id().clone()])
            .await
            .unwrap(),
        vec![
            format!("named:{}", music_root.display()),
            format!("named:{}", books_root.display()),
            format!("named:{}", books_root.join("nested").display()),
        ]
    );

    let unknown = LibraryId::from_uuid(Uuid::now_v7());
    assert!(matches!(
        registry
            .scopes_for_ids(&[music.id().clone(), unknown])
            .await,
        Err(LibraryRegistrationError::UnknownLibrary { .. })
    ));

    assert!(matches!(
        registry.scopes_for("unknown").await,
        Err(LibraryRegistrationError::UnknownLabel { .. })
    ));
    assert!(matches!(
        registry.scopes_for_numeric(99).await,
        Err(LibraryRegistrationError::UnknownNumericId { numeric_id: 99 })
    ));

    std::fs::remove_dir_all(&music_root).unwrap();
    std::fs::remove_dir_all(&books_root).unwrap();
}

#[sqlx::test]
async fn a_root_shared_by_two_labels_is_selectable_by_each(pool: PgPool) {
    let root = temp_root("shared");
    std::fs::copy(fixture(), root.join("a.flac")).unwrap();
    let root_text = root.to_str().unwrap();
    let registrations = [
        registration("music", &[root_text]),
        registration("books", &[root_text]),
    ];

    let registry = registry(&pool);
    let libraries = registry.register(&registrations).await.unwrap();
    assert_eq!(libraries.len(), 2);

    let scope = format!("named:{}", root.display());
    assert_eq!(
        registry.scopes_for("music").await.unwrap(),
        vec![scope.clone()]
    );
    assert_eq!(
        registry.scopes_for("books").await.unwrap(),
        vec![scope.clone()]
    );

    let roots = registry.scan_roots(&registrations);
    assert_eq!(roots, vec![root.clone()]);

    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&store) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&store) as Arc<dyn SourceLookup>;

    let first = scan(&catalogue, &sources, &roots).await.unwrap();
    assert_eq!(first.indexed, 1);
    let track_ids = scanned_track_ids(&pool, &scope).await;
    assert_eq!(track_ids.len(), 1);

    let again = scan(&catalogue, &sources, &roots).await.unwrap();
    assert_eq!(again.indexed, 0);
    assert_eq!(again.unchanged, 1);
    assert_eq!(scanned_track_ids(&pool, &scope).await, track_ids);

    std::fs::remove_dir_all(&root).unwrap();
}

#[sqlx::test]
async fn a_remount_does_not_change_public_ids(pool: PgPool) {
    let registry = registry(&pool);

    let first = registry
        .register(&[registration("music", &["/old/tank"])])
        .await
        .unwrap();
    let second = registry
        .register(&[registration("music", &["/new/tank"])])
        .await
        .unwrap();

    assert_eq!(first[0].id(), second[0].id());
    assert_eq!(first[0].numeric_id(), second[0].numeric_id());
    assert_eq!(
        registry.scopes_for("music").await.unwrap(),
        vec!["named:/new/tank".to_owned()]
    );
    assert_eq!(library_count(&pool).await, 1);
}

#[sqlx::test]
async fn a_stale_label_keeps_its_row(pool: PgPool) {
    let registry = registry(&pool);

    let both = registry
        .register(&[
            registration("music", &["/a"]),
            registration("books", &["/b"]),
        ])
        .await
        .unwrap();
    let books = both
        .iter()
        .find(|library| library.label() == "books")
        .unwrap()
        .clone();

    let current = registry
        .register(&[registration("music", &["/a"])])
        .await
        .unwrap();
    assert_eq!(current.len(), 2);

    let kept = current
        .iter()
        .find(|library| library.label() == "books")
        .unwrap();
    assert_eq!(kept.id(), books.id());
    assert_eq!(kept.numeric_id(), books.numeric_id());
    assert_eq!(
        registry.scopes_for("books").await.unwrap(),
        vec!["named:/b".to_owned()]
    );
}

#[sqlx::test]
async fn a_configured_label_replaces_its_root_set(pool: PgPool) {
    let registry = registry(&pool);

    registry
        .register(&[registration("music", &["/a", "/b"])])
        .await
        .unwrap();
    registry
        .register(&[registration("music", &["/a"])])
        .await
        .unwrap();

    assert_eq!(
        registry.scopes_for("music").await.unwrap(),
        vec!["named:/a".to_owned()]
    );
    assert_eq!(library_count(&pool).await, 1);
}

#[sqlx::test]
async fn a_racing_insert_of_the_same_label_and_roots_is_idempotent(pool: PgPool) {
    let store = PostgresLibraryStore::new(pool.clone());
    let roots = [PathBuf::from("/a"), PathBuf::from("/b")];

    let first = store.insert_library("music", &roots).await.unwrap();
    let second = store.insert_library("music", &roots).await.unwrap();

    assert_eq!(first.id(), second.id());
    assert_eq!(first.numeric_id(), second.numeric_id());
    assert_eq!(library_count(&pool).await, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM library_roots")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .scopes_for_ids(std::slice::from_ref(first.id()))
            .await
            .unwrap(),
        vec!["named:/a".to_owned(), "named:/b".to_owned()]
    );
}

#[sqlx::test]
async fn nested_roots_are_both_registered(pool: PgPool) {
    let registry = registry(&pool);
    let libraries = registry
        .register(&[registration("music", &["/tank/music", "/tank/music/live"])])
        .await
        .unwrap();

    assert_eq!(libraries.len(), 1);
    assert_eq!(
        registry.scopes_for("music").await.unwrap(),
        vec![
            "named:/tank/music".to_owned(),
            "named:/tank/music/live".to_owned()
        ]
    );
}
