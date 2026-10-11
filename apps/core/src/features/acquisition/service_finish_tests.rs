use super::*;
use crate::features::acquisition::domain::Attempt;
use crate::features::acquisition::service::{FinishBoundary, FinishHook};
use std::sync::atomic::Ordering;

fn hold_finish(
    harness: &mut Harness,
    boundary: FinishBoundary,
) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    let (hold, entered) = Hold::new();
    let once = Arc::new(Mutex::new(Some(hold)));
    let hook: FinishHook = Arc::new(move |current| {
        let hold = if current == boundary {
            once.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.wait().await;
            }
        })
    });
    Arc::get_mut(&mut harness.service)
        .unwrap()
        .set_finish_hook(hook);

    entered
}

async fn abort(handle: tokio::task::JoinHandle<Result<Step, AcquisitionError>>) {
    handle.abort();
    let result = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .unwrap();
    assert!(result.unwrap_err().is_cancelled());
}

async fn attempt(harness: &Harness, id: Uuid) -> Attempt {
    harness.store.load_attempt(id, 1).await.unwrap().unwrap()
}

fn imported_item() -> ImportedItem {
    ImportedItem {
        release_id: Some(release_id().to_string()),
        recording_id: None,
        path: PathBuf::from("/library/Album/One.flac"),
    }
}

async fn importing(harness: &Harness, relative: &str) -> Uuid {
    let id = seeded(
        harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![candidate(relative)],
    )
    .await;
    let path = staging_file(harness, relative);
    seed_attempt_with_delivered(harness, id, &[path]).await;
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![imported_item()],
        }));

    id
}

async fn transferring(harness: &Harness) -> Uuid {
    let id = seeded(
        harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Transferring,
        vec![candidate_with(&["One.flac", "Two.flac"])],
    )
    .await;
    seed_attempt(
        harness,
        id,
        vec![candidate_file("One.flac"), candidate_file("Two.flac")],
    )
    .await;
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Transferring,
                reason: None,
                handles: vec![Some("handle-0".to_owned()), Some("handle-1".to_owned())],
                delivered: Vec::new(),
                imported: Vec::new(),
            },
        )
        .await
        .unwrap();
    staging_file(harness, "Album/One.flac");

    id
}

fn mixed_status(second: Slot<TransferProgress>) -> Vec<Slot<TransferProgress>> {
    vec![
        Slot::Accepted(progress_with(
            TransferPhase::ReportedComplete,
            Some("Album/One.flac"),
        )),
        second,
    ]
}

#[sqlx::test]
async fn an_interrupted_partial_cancellation_replays_without_remote_records(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = transferring(&harness).await;
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(Slot::Accepted(progress(
            TransferPhase::Failed,
        )))));
    harness
        .transfer
        .observe_removed
        .store(true, Ordering::SeqCst);
    let (held, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("handle-1".to_owned(), held));
    let run = advance(&harness, id);
    entered(&mut cancelling).await;
    let stored = attempt(&harness, id).await;
    let intent = stored.finish_intent.unwrap();
    assert_eq!(intent.update.reason.as_deref(), Some("transfer_failed"));
    assert_eq!(
        intent.update.handles,
        vec![Some("handle-0".to_owned()), Some("handle-1".to_owned())]
    );
    assert_eq!(
        intent.update.delivered,
        vec![PathBuf::from("Album/One.flac")]
    );
    assert!(intent.cancel);
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac"]
    );
    assert_eq!(
        std::fs::read(harness.staging.join("Album/One.flac")).unwrap(),
        b"audio"
    );
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("handle-0")
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
    abort(run).await;

    let removed = harness
        .transfer
        .status(&[TransferHandle::new("handle-0".to_owned())])
        .await
        .unwrap();
    assert!(matches!(removed.as_slice(), [Slot::Missing]));
    let status_calls = harness.transfer.status_calls.load(Ordering::SeqCst);
    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(
        harness.transfer.status_calls.load(Ordering::SeqCst),
        status_calls,
        "finish resumes without rediscovering removed transfers"
    );
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Failed);
    assert_eq!(stored.delivered, vec![PathBuf::from("Album/One.flac")]);
    assert!(stored.finish_intent.is_none());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("handle-1")
    );
}

#[sqlx::test]
async fn an_unavailable_later_cancellation_resumes_the_saved_local_subset(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = transferring(&harness).await;
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(Slot::Missing)));
    harness
        .transfer
        .cancels
        .lock()
        .unwrap()
        .extend([Ok(()), Err(TransferError::Unavailable)]);
    harness
        .transfer
        .observe_removed
        .store(true, Ordering::SeqCst);

    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Unavailable)
    ));
    let intent = attempt(&harness, id).await.finish_intent.unwrap();
    assert_eq!(
        intent.update.delivered,
        vec![PathBuf::from("Album/One.flac")]
    );
    assert_eq!(
        intent.update.handles,
        vec![Some("handle-0".to_owned()), Some("handle-1".to_owned())]
    );
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("handle-0")
    );
    assert!(
        !harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("handle-1")
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
    assert_eq!(
        std::fs::read(harness.staging.join("Album/One.flac")).unwrap(),
        b"audio"
    );
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac"]
    );
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Malformed));

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(
        harness.transfer.status_calls.load(Ordering::SeqCst),
        1,
        "resume does not consult missing records"
    );
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("handle-1")
    );
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert!(attempt(&harness, id).await.finish_intent.is_none());
}

#[sqlx::test]
async fn partial_start_rejection_retains_accepted_handles_before_cancel(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = seeded(
        &harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        vec![
            candidate_with(&["One.flac", "Two.flac", "Three.flac"]),
            candidate("Next.flac"),
        ],
    )
    .await;
    harness.transfer.starts.lock().unwrap().push_back(Ok(vec![
        Slot::Accepted(TransferHandle::new("accepted-a".to_owned())),
        Slot::Accepted(TransferHandle::new("accepted-b".to_owned())),
        Slot::Rejected(Rejection::Refused),
    ]));
    let (held, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("accepted-b".to_owned(), held));
    let run = advance(&harness, id);
    entered(&mut cancelling).await;
    let intent = attempt(&harness, id).await.finish_intent.unwrap();
    let exact = vec![
        Some("accepted-a".to_owned()),
        Some("accepted-b".to_owned()),
        None,
    ];
    assert_eq!(intent.update.handles, exact);
    assert_eq!(intent.update.reason.as_deref(), Some("rejected"));
    assert!(intent.cancel);
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("accepted-a")
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 3);
    abort(run).await;

    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Failed);
    assert_eq!(stored.reason.as_deref(), Some("rejected"));
    assert_eq!(stored.handles, exact);
    assert!(stored.finish_intent.is_none());
    assert!(
        harness
            .transfer
            .removed
            .lock()
            .unwrap()
            .contains("accepted-b")
    );
    assert_eq!(
        harness.transfer.started_keys.lock().unwrap().len(),
        4,
        "only the successor starts on resume"
    );
    assert_eq!(
        harness
            .store
            .load_attempt(id, 2)
            .await
            .unwrap()
            .unwrap()
            .state,
        AttemptState::Transferring
    );
    assert!(harness.importer.seen.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn an_imported_attempt_finishes_its_cleanup_on_resume(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = importing(&harness, "Album/One.flac").await;
    let private = harness.staging.join(".whio-import").join(format!("{id}-1"));
    std::fs::create_dir_all(&private).unwrap();
    std::fs::hard_link(
        harness.staging.join("Album/One.flac"),
        private.join("One.flac"),
    )
    .unwrap();
    harness
        .store
        .update_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Imported,
                reason: None,
                handles: vec![Some("handle-0".to_owned())],
                delivered: vec![PathBuf::from("Album/One.flac")],
                imported: vec![imported_item()],
            },
        )
        .await
        .unwrap();

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(!private.exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.imported.len(), 1);
    assert_eq!(stored.imported[0].release_id, imported_item().release_id);
    assert!(stored.finish_intent.is_none());
    assert!(
        harness.importer.seen.lock().unwrap().is_empty(),
        "imported resumption does not reimport"
    );
}

#[sqlx::test]
async fn interruption_before_import_outcome_intent_preserves_input(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let mut harness = Harness::new(pool, staging, Clock::system()).await;
    let mut held = hold_finish(&mut harness, FinishBoundary::BeforeIntent);
    let id = importing(&harness, "Album/One.flac").await;
    let run = advance(&harness, id);
    entered(&mut held).await;
    assert_eq!(
        harness.importer.seen.lock().unwrap().len(),
        1,
        "the importer returned a real outcome before the hold"
    );
    let private = harness.importer.seen.lock().unwrap()[0].0.join("One.flac");
    assert!(attempt(&harness, id).await.finish_intent.is_none());
    assert_eq!(state(&harness, id).await, RequestState::Importing);
    assert_eq!(
        std::fs::read(harness.staging.join("Album/One.flac")).unwrap(),
        b"audio"
    );
    assert_eq!(std::fs::read(&private).unwrap(), b"audio");
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac"]
    );
    abort(run).await;
    *harness.ownership.owned.lock().unwrap() = true;

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert_eq!(harness.importer.seen.lock().unwrap().len(), 1);
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(!private.parent().unwrap().exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn interruption_before_finish_commit_resumes_the_persisted_outcome(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let mut harness = Harness::new(pool, staging, Clock::system()).await;
    let mut held = hold_finish(&mut harness, FinishBoundary::BeforeCommit);
    let id = importing(&harness, "Album/One.flac").await;
    let run = advance(&harness, id);
    entered(&mut held).await;
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Importing);
    assert!(
        stored.imported.is_empty(),
        "final outcome has not been persisted"
    );
    let intent = stored.finish_intent.unwrap();
    assert_eq!(intent.request_state, RequestState::Completed);
    assert_eq!(intent.update.imported.len(), 1);
    assert_eq!(
        intent.update.imported[0].release_id,
        imported_item().release_id
    );
    assert_eq!(
        intent.update.delivered,
        vec![PathBuf::from("Album/One.flac")]
    );
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac"],
        "claims are retained until the atomic finish commit"
    );
    abort(run).await;

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert_eq!(
        harness.importer.seen.lock().unwrap().len(),
        1,
        "persisted outcome resumes without another import"
    );
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Imported);
    assert_eq!(stored.imported.len(), 1);
    assert!(stored.finish_intent.is_none());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn interruption_before_transfer_finish_intent_does_not_cancel(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let mut harness = Harness::new(pool, staging, Clock::system()).await;
    let mut held = hold_finish(&mut harness, FinishBoundary::BeforeIntent);
    let id = transferring(&harness).await;
    let statuses = mixed_status(Slot::Missing);
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(statuses));
    let run = advance(&harness, id);
    entered(&mut held).await;
    assert!(attempt(&harness, id).await.finish_intent.is_none());
    assert!(harness.transfer.cancelled.lock().unwrap().is_empty());
    assert!(harness.transfer.removed.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(harness.staging.join("Album/One.flac")).unwrap(),
        b"audio"
    );
    abort(run).await;
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(Slot::Missing)));

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_failed_finish_commit_retains_its_intent_and_claims(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
    let id = importing(&harness, "Album/One.flac").await;
    sqlx::raw_sql(
        "CREATE FUNCTION reject_acquisition_completion() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.state = 'completed' THEN RAISE EXCEPTION 'held completion failure'; \
         END IF; RETURN NEW; END $$; \
         CREATE TRIGGER reject_acquisition_completion BEFORE UPDATE ON acquisition_requests \
         FOR EACH ROW EXECUTE FUNCTION reject_acquisition_completion()",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Repository(_))
    ));
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Importing);
    assert!(stored.imported.is_empty());
    assert!(
        stored.finish_intent.is_some(),
        "the failed transaction does not clear its durable intent"
    );
    assert_eq!(state(&harness, id).await, RequestState::Importing);
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec!["Album/One.flac"]
    );
    assert!(!harness.staging.join("Album/One.flac").exists());
    sqlx::raw_sql("DROP TRIGGER reject_acquisition_completion ON acquisition_requests; DROP FUNCTION reject_acquisition_completion()")
        .execute(&pool).await.unwrap();

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert_eq!(harness.importer.seen.lock().unwrap().len(), 1);
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Imported);
    assert_eq!(stored.imported.len(), 1);
    assert!(stored.finish_intent.is_none());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_failed_finish_intent_write_prevents_external_cancellation(pool: PgPool) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool.clone(), staging, Clock::system()).await;
    let id = transferring(&harness).await;
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(Slot::Missing)));
    sqlx::raw_sql(
        "CREATE FUNCTION reject_acquisition_intent() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.finish_intent IS NOT NULL THEN RAISE EXCEPTION 'held intent failure'; \
         END IF; RETURN NEW; END $$; \
         CREATE TRIGGER reject_acquisition_intent BEFORE UPDATE ON acquisition_attempts \
         FOR EACH ROW EXECUTE FUNCTION reject_acquisition_intent()",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Repository(_))
    ));
    assert!(attempt(&harness, id).await.finish_intent.is_none());
    assert!(harness.transfer.cancelled.lock().unwrap().is_empty());
    assert!(harness.transfer.removed.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(harness.staging.join("Album/One.flac")).unwrap(),
        b"audio"
    );
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
    sqlx::raw_sql("DROP TRIGGER reject_acquisition_intent ON acquisition_attempts; DROP FUNCTION reject_acquisition_intent()")
        .execute(&pool).await.unwrap();
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(Slot::Missing)));

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

async fn mixed_cleanup(pool: PgPool, failed: Slot<TransferProgress>) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let id = transferring(&harness).await;
    staging_file(&harness, "Other/Keep.flac");
    harness
        .transfer
        .statuses
        .lock()
        .unwrap()
        .push_back(Ok(mixed_status(failed)));

    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    let stored = attempt(&harness, id).await;
    assert_eq!(stored.state, AttemptState::Failed);
    assert_eq!(stored.reason.as_deref(), Some("transfer_failed"));
    assert_eq!(stored.delivered, vec![PathBuf::from("Album/One.flac")]);
    assert!(stored.finish_intent.is_none());
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    assert_eq!(harness.transfer.cancelled.lock().unwrap().len(), 2);
    assert!(!harness.staging.join("Album/One.flac").exists());
    assert!(!harness.staging.join("Album").exists());
    assert_eq!(
        std::fs::read(harness.staging.join("Other/Keep.flac")).unwrap(),
        b"audio"
    );
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert!(harness.importer.seen.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn a_mixed_complete_and_failed_transfer_cleans_its_local_subset(pool: PgPool) {
    mixed_cleanup(pool, Slot::Accepted(progress(TransferPhase::Failed))).await;
}

#[sqlx::test]
async fn a_mixed_complete_and_missing_transfer_cleans_its_local_subset(pool: PgPool) {
    mixed_cleanup(pool, Slot::Missing).await;
}

async fn held_album(pool: PgPool, prefix: &str) {
    let staging = std::env::temp_dir().join(format!("whio-aq-finish-{}", Uuid::now_v7()));
    let harness = Harness::new(pool, staging, Clock::system()).await;
    let one = format!("{prefix}One.flac");
    let two = format!("{prefix}Two.flac");
    let id = importing(&harness, &one).await;
    staging_file(&harness, "Other/Keep.flac");
    let (held, mut importing) = Hold::new();
    *harness.importer.hold.lock().unwrap() = Some(Arc::clone(&held));
    let run = advance(&harness, id);
    entered(&mut importing).await;
    let private = harness.importer.seen.lock().unwrap()[0].0.clone();
    assert_eq!(std::fs::read(private.join("One.flac")).unwrap(), b"audio");
    let other = Uuid::now_v7();
    harness
        .store
        .insert_request(
            other,
            &AcquisitionOrigin::Release(MbId::new(Uuid::now_v7())),
        )
        .await
        .unwrap();
    harness
        .store
        .set_target(
            other,
            &AcquisitionTarget::Release(release_id()),
            "An Artist",
            "Another Album",
            &expected_track(),
        )
        .await
        .unwrap();
    harness
        .store
        .set_candidates(other, &[candidate(&two)])
        .await
        .unwrap();
    harness
        .store
        .set_state(other, RequestState::Searching)
        .await
        .unwrap();
    *harness.transfer.on_status.lock().unwrap() =
        Some((harness.staging.join(&two), b"another download".to_vec()));
    assert_eq!(
        joined(advance(&harness, other)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(state(&harness, other).await, RequestState::Transferring);
    assert_eq!(
        std::fs::read(harness.staging.join(&two)).unwrap(),
        b"another download"
    );
    assert!(
        !run.is_finished(),
        "the importer is explicitly held while the other transfer writes"
    );
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec![one.clone()]
    );
    assert!(
        harness
            .store
            .claimed_paths(other, 1)
            .await
            .unwrap()
            .is_empty()
    );
    held.release(1);

    assert_eq!(joined(run).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert_eq!(
        *harness.importer.after_hold.lock().unwrap(),
        vec![vec!["One.flac".to_owned()]],
        "the held importer never sees the newly downloaded audio"
    );
    assert!(!harness.staging.join(&one).exists());
    assert!(!private.exists());
    assert_eq!(
        std::fs::read(harness.staging.join(&two)).unwrap(),
        b"another download"
    );
    assert_eq!(
        std::fs::read(harness.staging.join("Other/Keep.flac")).unwrap(),
        b"audio"
    );
    assert!(harness.staging.is_dir());
    assert!(harness.staging.join("Other").is_dir());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert_eq!(state(&harness, other).await, RequestState::Transferring);
}

#[sqlx::test]
async fn a_held_album_import_is_isolated_from_another_download(pool: PgPool) {
    held_album(pool, "Album/").await;
}

#[sqlx::test]
async fn a_root_level_album_import_preserves_the_root_and_other_subtree(pool: PgPool) {
    held_album(pool, "").await;
}
