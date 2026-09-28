use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::features::identity::domain::{Principal, UserId, UserRole};
use crate::features::music_library::{
    catalogue::LibraryService,
    domain::{
        PlaylistEntryChange, PlaylistId, PlaylistMetadataPatch, ProviderId, SourceIdentity,
        SourceScope, TrackId, TrackMetadata,
    },
    playlists::{PlaylistEdit, PlaylistError, PlaylistService},
    storage::contracts::{
        CatalogueWrite, LibraryRead, PlaylistStore, SourceLookup, StateStore, TrackRepositoryError,
    },
    test_support::PostgresTrackRepository,
};

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

fn service(store: &Arc<PostgresTrackRepository>) -> PlaylistService {
    let library = Arc::new(LibraryService::new(
        Arc::clone(store) as Arc<dyn LibraryRead>,
        Arc::clone(store) as Arc<dyn SourceLookup>,
        Arc::clone(store) as Arc<dyn StateStore>,
    ));

    PlaylistService::new(Arc::clone(store) as Arc<dyn PlaylistStore>, library)
}

async fn track(store: &Arc<PostgresTrackRepository>, name: &str) -> TrackId {
    store
        .get_or_create(local_source(&format!("{name}.flac")), metadata(name))
        .await
        .unwrap()
        .id()
        .clone()
}

fn edit<'a>(name: Option<&'a str>, additions: &'a [TrackId]) -> PlaylistEdit<'a> {
    PlaylistEdit {
        name,
        comment: None,
        public: None,
        additions,
        removals: Vec::new(),
    }
}

fn metadata_patch(name: Option<&str>) -> PlaylistMetadataPatch {
    PlaylistMetadataPatch {
        name: name.map(str::to_owned),
        ..PlaylistMetadataPatch::default()
    }
}

fn admin(user: Uuid) -> Principal {
    Principal {
        user_id: UserId::from_uuid(user),
        role: UserRole::Admin,
    }
}

fn member(user: Uuid) -> Principal {
    Principal {
        user_id: UserId::from_uuid(user),
        role: UserRole::Member,
    }
}

struct Fixture {
    store: Arc<PostgresTrackRepository>,
    service: PlaylistService,
    owner: Uuid,
    target: Uuid,
    admin: Uuid,
}

async fn fixture(pool: PgPool) -> Fixture {
    let store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let provision = crate::features::identity::provision(pool.clone());
    let owner = provision
        .create_user("transfer-owner", "Owner")
        .await
        .unwrap();
    let target = provision
        .create_user("transfer-target", "Target")
        .await
        .unwrap();
    let admin = provision
        .create_user("transfer-admin", "Admin")
        .await
        .unwrap();
    provision.assign_admin("transfer-admin").await.unwrap();

    Fixture {
        service: service(&store),
        store,
        owner,
        target,
        admin,
    }
}

impl Fixture {
    async fn shared_playlist(&self) -> (PlaylistId, TrackId) {
        let first = track(&self.store, "First").await;
        let playlist = self
            .service
            .create(self.owner, "road trip", None)
            .await
            .unwrap();
        assert!(
            self.service
                .add_track(self.owner, playlist.id(), &first)
                .await
                .unwrap()
        );
        self.service
            .update(self.owner, playlist.id(), "road trip", None, true)
            .await
            .unwrap()
            .unwrap();

        (playlist.id().clone(), first)
    }
}

#[sqlx::test]
async fn an_admin_transfer_hands_read_and_edit_to_the_new_owner(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, first) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let transferred = fixture
        .service
        .transfer_owner(
            &admin(fixture.admin),
            &playlist_id,
            UserId::from_uuid(fixture.target),
        )
        .await
        .unwrap();

    assert_eq!(transferred.owner_id(), fixture.target);
    assert_eq!(transferred.owner_username(), "transfer-target");
    assert_eq!(
        transferred.revision(),
        before.revision(),
        "a transfer changes the owner, not the entries"
    );
    assert_eq!(transferred.name(), before.name());

    let owned = fixture
        .service
        .detail(fixture.target, &playlist_id)
        .await
        .unwrap()
        .expect("the new owner reads the playlist");
    assert!(owned.editable);
    assert_eq!(owned.playlist.owner_id(), fixture.target);
    assert!(
        fixture
            .service
            .edit(fixture.target, &playlist_id, edit(Some("mine now"), &[]))
            .await
            .unwrap(),
        "the new owner edits the playlist"
    );
    assert_eq!(
        fixture
            .service
            .detail(fixture.target, &playlist_id)
            .await
            .unwrap()
            .unwrap()
            .playlist
            .name(),
        "mine now"
    );

    let shared = fixture
        .service
        .detail(fixture.owner, &playlist_id)
        .await
        .unwrap()
        .expect("the public playlist stays readable by its former owner");
    assert!(!shared.editable);
    assert_eq!(
        shared.playlist.owner_id(),
        fixture.target,
        "the read reports the new owner, not the caller"
    );
    assert!(
        !fixture
            .service
            .edit(fixture.owner, &playlist_id, edit(Some("hijack"), &[]))
            .await
            .unwrap(),
        "the former owner can no longer edit"
    );
    assert!(
        !fixture
            .service
            .add_track(fixture.owner, &playlist_id, &first)
            .await
            .unwrap(),
        "the former owner cannot add to the playlist either"
    );
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap()
            .name(),
        "mine now",
        "the refused edits changed nothing"
    );

    assert_eq!(fixture.service.list(fixture.target).await.unwrap().len(), 1);
}

#[sqlx::test]
async fn a_non_admin_transfer_is_refused_and_changes_nothing(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let error = fixture
        .service
        .transfer_owner(
            &member(fixture.owner),
            &playlist_id,
            UserId::from_uuid(fixture.target),
        )
        .await
        .unwrap_err();

    assert!(matches!(error, PlaylistError::Forbidden));
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before,
        "a refused transfer writes nothing"
    );
    let read = fixture
        .service
        .detail(fixture.target, &playlist_id)
        .await
        .unwrap()
        .expect("the unchanged playlist is still public");
    assert_eq!(read.playlist.owner_id(), fixture.owner);
    assert!(!read.editable);
}

#[sqlx::test]
async fn a_transfer_to_the_current_owner_reports_the_playlist_unchanged(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let transferred = fixture
        .service
        .transfer_owner(
            &admin(fixture.admin),
            &playlist_id,
            UserId::from_uuid(fixture.owner),
        )
        .await
        .unwrap();

    assert_eq!(
        transferred, before,
        "a transfer to the current owner reports the playlist unchanged"
    );
    assert_eq!(transferred.revision(), before.revision());
    assert_eq!(transferred.updated_at(), before.updated_at());
    assert_eq!(
        fixture
            .service
            .detail(fixture.owner, &playlist_id)
            .await
            .unwrap()
            .unwrap()
            .playlist,
        before
    );
}

#[sqlx::test]
async fn an_admin_edits_a_playlist_they_do_not_own_but_a_non_admin_still_cannot(pool: PgPool) {
    let fixture = fixture(pool).await;
    let first = track(&fixture.store, "First").await;
    let second = track(&fixture.store, "Second").await;
    let playlist = fixture
        .service
        .create(fixture.owner, "private", None)
        .await
        .unwrap();
    assert!(
        fixture
            .service
            .add_track(fixture.owner, playlist.id(), &first)
            .await
            .unwrap()
    );

    let edited = fixture
        .service
        .write_at_revision_as(
            &admin(fixture.admin),
            playlist.id(),
            None,
            metadata_patch(Some("curated")),
            PlaylistEntryChange::Apply {
                removals: Vec::new(),
                additions: vec![second.clone()],
            },
        )
        .await
        .unwrap()
        .expect("an admin edits a playlist they do not own");
    assert_eq!(edited.name(), "curated");
    assert_eq!(edited.track_count(), 2);
    assert_eq!(edited.owner_id(), fixture.owner);

    assert!(
        fixture
            .service
            .write_at_revision_as(
                &member(fixture.target),
                playlist.id(),
                None,
                metadata_patch(Some("hijack")),
                PlaylistEntryChange::Apply {
                    removals: Vec::new(),
                    additions: vec![second.clone()],
                },
            )
            .await
            .unwrap()
            .is_none(),
        "a non-admin still cannot edit a playlist they do not own"
    );
    let after = fixture
        .store
        .find_playlist(playlist.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.name(), "curated");
    assert_eq!(after.track_count(), 2);

    assert!(
        fixture
            .service
            .write_at_revision(
                fixture.admin,
                playlist.id(),
                None,
                metadata_patch(Some("owner only")),
                PlaylistEntryChange::Unchanged,
            )
            .await
            .unwrap()
            .is_none(),
        "the owner-only write does not admit an admin"
    );
    assert!(
        fixture
            .service
            .edit(fixture.owner, playlist.id(), edit(Some("theirs"), &[]))
            .await
            .unwrap(),
        "the owner's own edit path is unchanged"
    );

    let error = fixture
        .service
        .write_at_revision_as(
            &admin(fixture.admin),
            playlist.id(),
            Some(after.revision() + 5),
            metadata_patch(Some("stale")),
            PlaylistEntryChange::Unchanged,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, PlaylistError::Conflict { .. }),
        "the admin entry point keeps the revision contract"
    );
}

#[sqlx::test]
async fn a_failed_transfer_leaves_the_previous_owner(pool: PgPool) {
    let fixture = fixture(pool).await;
    let (playlist_id, _) = fixture.shared_playlist().await;
    let before = fixture
        .store
        .find_playlist(&playlist_id)
        .await
        .unwrap()
        .unwrap();

    let error = fixture
        .store
        .transfer_playlist_owner(&playlist_id, Uuid::now_v7())
        .await
        .unwrap_err();
    assert!(matches!(error, TrackRepositoryError::InvalidData));
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before,
        "a failed update leaves the previous owner intact"
    );

    let error = fixture
        .service
        .transfer_owner(
            &admin(fixture.admin),
            &PlaylistId::from_uuid(Uuid::now_v7()),
            UserId::from_uuid(fixture.target),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, PlaylistError::NotFound));
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );

    assert!(
        fixture
            .service
            .transfer_owner(
                &member(fixture.owner),
                &playlist_id,
                UserId::from_uuid(fixture.target)
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .store
            .find_playlist(&playlist_id)
            .await
            .unwrap()
            .unwrap(),
        before
    );
}
