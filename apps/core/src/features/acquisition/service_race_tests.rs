use super::*;

async fn race(pool: PgPool, width: usize) -> (Harness, Arc<Mutex<chrono::DateTime<Utc>>>) {
    let now = Arc::new(Mutex::new(Utc::now()));
    let clock = {
        let now = Arc::clone(&now);
        Clock::from_fn(move || *now.lock().unwrap())
    };
    let staging = std::env::temp_dir().join(format!("whio-aq-race-{}", Uuid::now_v7()));
    let harness = Harness::build_with_options(pool, staging, clock, None, None, width, None).await;
    (harness, now)
}

async fn request(harness: &Harness, users: &[&str]) -> Uuid {
    let candidates = users
        .iter()
        .map(|user| candidate_from(user, &[&format!("{user}.flac")]))
        .collect();
    seeded(
        harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Searching,
        candidates,
    )
    .await
}

fn starts(harness: &Harness, users: &[&str]) {
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .extend(users.iter().map(|user| {
            Ok(vec![Slot::Accepted(TransferHandle::new(
                (*user).to_owned(),
            ))])
        }));
}

fn statuses(harness: &Harness, progresses: &[TransferProgress]) {
    harness.transfer.statuses.lock().unwrap().extend(
        progresses
            .iter()
            .map(|progress| Ok(vec![Slot::Accepted(progress.clone())])),
    );
}

fn silent(harness: &Harness, count: usize) {
    statuses(harness, &vec![progress(TransferPhase::Queued); count]);
}

fn bytes() -> TransferProgress {
    let mut value = progress(TransferPhase::Queued);
    value.bytes_done = Some(1);
    value
}

async fn held_poll(harness: &Harness, id: Uuid, started: usize) -> Step {
    let (hold, mut observed) = Hold::new();
    *harness.transfer.status_hold.lock().unwrap() = Some(Arc::clone(&hold));
    let run = advance(harness, id);
    entered(&mut observed).await;
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), started);
    assert!(!run.is_finished());
    hold.release(1);
    joined(run).await.unwrap()
}

async fn complete(harness: &Harness, id: Uuid, path: &str) {
    staging_file(harness, path);
    statuses(
        harness,
        &[progress_with(TransferPhase::ReportedComplete, Some(path))],
    );
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/One.flac"),
            }],
        }));
    assert_eq!(joined(advance(harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(harness, id).await, RequestState::Completed);
    assert!(!harness.staging.join(path).exists());
}

async fn attempt(
    harness: &Harness,
    id: Uuid,
    number: u16,
) -> crate::features::acquisition::domain::Attempt {
    harness
        .store
        .load_attempt(id, number)
        .await
        .unwrap()
        .unwrap()
}

async fn abort(run: tokio::task::JoinHandle<Result<Step, AcquisitionError>>) {
    run.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
}

async fn same_path_tie(pool: PgPool, restart: bool) {
    let (mut harness, _) = race(pool, 2).await;
    let id = request(&harness, &["a", "b"]).await;
    starts(&harness, &["a", "b"]);
    let path = staging_file(&harness, "Album/One.flac");
    let completed = progress_with(TransferPhase::ReportedComplete, Some(&path));
    statuses(&harness, &[completed.clone(), completed.clone()]);
    harness
        .importer
        .outcomes
        .lock()
        .unwrap()
        .push_back(Ok(ImportOutcome {
            items: vec![ImportedItem {
                release_id: Some(release_id().to_string()),
                recording_id: None,
                path: PathBuf::from("/library/One.flac"),
            }],
        }));
    let (cancel, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("b".to_owned(), Arc::clone(&cancel)));
    let (import, mut importing) = Hold::new();
    *harness.importer.hold.lock().unwrap() = Some(Arc::clone(&import));
    let mut run = advance(&harness, id);
    entered(&mut cancelling).await;
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .winner,
        Some(1)
    );
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec![path.clone()]
    );
    assert!(harness.store.claimed_paths(id, 2).await.unwrap().is_empty());
    assert!(harness.staging.join(&path).exists());
    assert!(attempt(&harness, id, 2).await.finish_intent.is_some());
    assert!(harness.importer.seen.lock().unwrap().is_empty());
    if restart {
        abort(run).await;
        harness.restart();
        statuses(&harness, &[completed]);
        run = advance(&harness, id);
    } else {
        cancel.release(1);
    }
    entered(&mut importing).await;
    assert_eq!(
        attempt(&harness, id, 2).await.reason.as_deref(),
        Some("lost_race")
    );
    assert!(attempt(&harness, id, 2).await.finish_intent.is_none());
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec![path.clone()]
    );
    assert!(harness.store.claimed_paths(id, 2).await.unwrap().is_empty());
    assert!(harness.staging.join(&path).exists());
    assert_eq!(harness.importer.seen.lock().unwrap().len(), 1);
    import.release(1);
    assert_eq!(joined(run).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Completed);
    assert_eq!(harness.importer.seen.lock().unwrap().len(), 1);
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 2);
    assert!(!harness.staging.join(path).exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_same_path_completion_tie_protects_the_winner_input(pool: PgPool) {
    same_path_tie(pool, false).await;
}

#[sqlx::test]
async fn a_same_path_tie_survives_interrupted_loser_cancellation(pool: PgPool) {
    same_path_tie(pool, true).await;
}

#[sqlx::test]
async fn the_first_bytes_win_and_the_rest_lose(pool: PgPool) {
    let (harness, _) = race(pool, 3).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a", "b", "c"]);
    silent(&harness, 3);
    assert_eq!(
        held_poll(&harness, id, 3).await,
        Step::Wait(Duration::from_secs(10))
    );
    let loser = staging_file(&harness, "C/c.flac");
    statuses(
        &harness,
        &[
            progress(TransferPhase::Queued),
            bytes(),
            progress_with(TransferPhase::ReportedComplete, Some(&loser)),
        ],
    );
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    let stored = harness.store.find_request(id).await.unwrap().unwrap();
    assert_eq!(stored.attempts, 3);
    assert_eq!(stored.round.unwrap().winner, Some(2));
    for number in [1, 3] {
        let attempt = attempt(&harness, id, number).await;
        assert_eq!(attempt.state, AttemptState::Failed);
        assert_eq!(attempt.reason.as_deref(), Some("lost_race"));
        assert!(attempt.finish_intent.is_none());
        assert!(
            harness
                .store
                .claimed_paths(id, number)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(
        *harness.transfer.cancelled.lock().unwrap(),
        vec![("a".to_owned(), true), ("c".to_owned(), true)]
    );
    assert!(!harness.staging.join(loser).exists());
    assert!(harness.importer.seen.lock().unwrap().is_empty());
    complete(&harness, id, "B/b.flac").await;
    assert_eq!(attempt(&harness, id, 2).await.state, AttemptState::Imported);
    assert_eq!(harness.importer.seen.lock().unwrap().len(), 1);
}

#[sqlx::test]
async fn a_silent_round_times_out_and_the_next_round_starts(pool: PgPool) {
    let (harness, now) = race(pool, 3).await;
    let id = request(&harness, &["a", "b", "c", "d"]).await;
    starts(&harness, &["a", "b", "c", "d"]);
    silent(&harness, 3);
    assert_eq!(
        held_poll(&harness, id, 3).await,
        Step::Wait(Duration::from_secs(10))
    );
    *now.lock().unwrap() += chrono::Duration::seconds(600);
    silent(&harness, 4);
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    for number in 1..=3 {
        assert_eq!(
            attempt(&harness, id, number).await.reason.as_deref(),
            Some("start_timeout")
        );
    }
    let stored = harness.store.find_request(id).await.unwrap().unwrap();
    let round = stored.round.unwrap();
    assert_eq!(round.attempts, vec![4]);
    assert_eq!(round.started_at, *now.lock().unwrap());
    assert_eq!(round.winner, None);
    assert_eq!(harness.transfer.cancelled.lock().unwrap().len(), 3);
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 4);
}

#[sqlx::test]
async fn users_are_never_queued_twice(pool: PgPool) {
    let (harness, _) = race(pool, 1).await;
    let id = request(&harness, &["a", "a", "a", "b"]).await;
    starts(&harness, &["a", "b"]);
    silent(&harness, 1);
    assert_eq!(
        held_poll(&harness, id, 1).await,
        Step::Wait(Duration::from_secs(10))
    );
    statuses(
        &harness,
        &[
            progress(TransferPhase::Failed),
            progress(TransferPhase::Queued),
        ],
    );
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    statuses(&harness, &[progress(TransferPhase::Failed)]);
    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
    assert!(harness.store.load_attempt(id, 3).await.unwrap().is_none());
    assert_eq!(attempt(&harness, id, 4).await.state, AttemptState::Failed);
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 2);
}

#[sqlx::test]
async fn the_transfer_timeout_starts_at_the_win(pool: PgPool) {
    let (mut harness, now) = race(pool, 2).await;
    let start = *now.lock().unwrap();
    let id = request(&harness, &["a", "b"]).await;
    starts(&harness, &["a", "b"]);
    silent(&harness, 2);
    assert_eq!(
        held_poll(&harness, id, 2).await,
        Step::Wait(Duration::from_secs(10))
    );
    *now.lock().unwrap() = start + chrono::Duration::seconds(599);
    statuses(&harness, &[bytes(), progress(TransferPhase::Queued)]);
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .won_at,
        Some(*now.lock().unwrap())
    );
    harness.restart();
    *now.lock().unwrap() = start + chrono::Duration::seconds(1800);
    statuses(&harness, &[progress(TransferPhase::Active)]);
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(
        attempt(&harness, id, 1).await.state,
        AttemptState::Transferring
    );
    *now.lock().unwrap() = start + chrono::Duration::seconds(2399);
    statuses(&harness, &[progress(TransferPhase::Active)]);
    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(
        attempt(&harness, id, 1).await.reason.as_deref(),
        Some("transfer_timeout")
    );
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 2);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
}

#[sqlx::test]
async fn a_restart_before_a_winner_resumes_the_round(pool: PgPool) {
    let (mut harness, _) = race(pool, 3).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a", "b", "c"]);
    silent(&harness, 3);
    assert_eq!(
        held_poll(&harness, id, 3).await,
        Step::Wait(Duration::from_secs(10))
    );
    let keys = harness.transfer.started_keys.lock().unwrap().clone();
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    harness.restart();
    statuses(
        &harness,
        &[
            progress(TransferPhase::Queued),
            progress(TransferPhase::Queued),
            bytes(),
        ],
    );
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    let resumed = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(resumed.started_at, round.started_at);
    assert_eq!(resumed.attempts, round.attempts);
    assert_eq!(resumed.winner, Some(3));
    assert_eq!(*harness.transfer.started_keys.lock().unwrap(), keys);
    complete(&harness, id, "C/c.flac").await;
}

#[sqlx::test]
async fn a_restart_while_losers_finish_completes_their_cleanup(pool: PgPool) {
    let (mut harness, _) = race(pool, 3).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a", "b", "c"]);
    silent(&harness, 3);
    assert_eq!(
        held_poll(&harness, id, 3).await,
        Step::Wait(Duration::from_secs(10))
    );
    staging_file(&harness, "B/b.flac");
    staging_file(&harness, "C/c.flac");
    statuses(
        &harness,
        &[
            bytes(),
            progress_with(TransferPhase::ReportedComplete, Some("B/b.flac")),
            progress_with(TransferPhase::ReportedComplete, Some("C/c.flac")),
        ],
    );
    let (hold, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("c".to_owned(), hold));
    let run = advance(&harness, id);
    entered(&mut cancelling).await;
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .winner,
        Some(1)
    );
    assert_eq!(
        attempt(&harness, id, 2).await.reason.as_deref(),
        Some("lost_race")
    );
    assert!(attempt(&harness, id, 2).await.finish_intent.is_none());
    let intent = attempt(&harness, id, 3).await.finish_intent.unwrap();
    assert_eq!(intent.update.reason.as_deref(), Some("lost_race"));
    assert_eq!(intent.update.delivered, vec![PathBuf::from("C/c.flac")]);
    assert!(intent.cancel);
    assert!(!harness.staging.join("B/b.flac").exists());
    assert!(harness.staging.join("C/c.flac").exists());
    assert_eq!(
        harness.store.claimed_paths(id, 3).await.unwrap(),
        vec!["C/c.flac"]
    );
    assert_eq!(
        harness
            .transfer
            .status_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        6
    );
    run.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    harness
        .transfer
        .observe_removed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    harness.restart();
    statuses(&harness, &[progress(TransferPhase::Active)]);
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert!(!harness.staging.join("C/c.flac").exists());
    assert!(harness.store.claimed_paths(id, 3).await.unwrap().is_empty());
    assert!(attempt(&harness, id, 3).await.finish_intent.is_none());
    assert_eq!(
        harness
            .transfer
            .status_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        7
    );
    assert_eq!(
        *harness.transfer.cancelled.lock().unwrap(),
        vec![
            ("b".to_owned(), true),
            ("c".to_owned(), true),
            ("c".to_owned(), true)
        ]
    );
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 3);
    complete(&harness, id, "A/a.flac").await;
}

#[sqlx::test]
async fn width_one_matches_single_attempt_behaviour(pool: PgPool) {
    let (harness, _) = race(pool, 1).await;
    let id = request(&harness, &["a", "b"]).await;
    starts(&harness, &["a"]);
    silent(&harness, 1);
    assert_eq!(
        held_poll(&harness, id, 1).await,
        Step::Wait(Duration::from_secs(10))
    );
    statuses(&harness, &[progress(TransferPhase::Active)]);
    assert_eq!(
        joined(advance(&harness, id)).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .winner,
        Some(1)
    );
    complete(&harness, id, "A/a.flac").await;
    assert!(harness.transfer.cancelled.lock().unwrap().is_empty());
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 1);
    assert!(harness.store.load_attempt(id, 2).await.unwrap().is_none());
}

#[sqlx::test]
async fn a_restart_during_round_start_reuses_only_unpersisted_handles(pool: PgPool) {
    let (mut harness, _) = race(pool, 3).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a"]);
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));
    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Unavailable)
    ));
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    let first = harness.transfer.started_keys.lock().unwrap().clone();
    assert_eq!(first.len(), 2);
    assert_eq!(round.attempts, vec![1, 2, 3]);
    assert_eq!(
        attempt(&harness, id, 1).await.handles,
        vec![Some("a".to_owned())]
    );
    assert!(attempt(&harness, id, 2).await.handles.is_empty());
    assert!(attempt(&harness, id, 3).await.handles.is_empty());
    harness.restart();
    starts(&harness, &["b", "c"]);
    silent(&harness, 3);
    assert_eq!(
        held_poll(&harness, id, 4).await,
        Step::Wait(Duration::from_secs(10))
    );
    let resumed = harness.transfer.started_keys.lock().unwrap().clone();
    assert_eq!(resumed[0], first[0]);
    assert_eq!(resumed[1], resumed[2]);
    assert_eq!(resumed.iter().copied().collect::<HashSet<_>>().len(), 3);
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .started_at,
        round.started_at
    );
}

async fn legacy_failed(harness: &Harness) -> (Uuid, String, PathBuf) {
    let id = seeded(
        harness,
        AcquisitionTarget::Release(release_id()),
        RequestState::Importing,
        vec![
            candidate_from("a", &["One.flac"]),
            candidate_from("b", &["Two.flac"]),
        ],
    )
    .await;
    let one = staging_file(harness, "Album/One.flac");
    seed_attempt_with_delivered(harness, id, std::slice::from_ref(&one)).await;
    harness
        .store
        .record_attempt(
            id,
            1,
            &AttemptUpdate {
                state: AttemptState::Failed,
                reason: Some("import_skipped".to_owned()),
                handles: vec![Some("a".to_owned())],
                delivered: vec![PathBuf::from(&one)],
                imported: Vec::new(),
            },
            RequestState::Transferring,
            None,
        )
        .await
        .unwrap();

    let private = harness.staging.join(".whio-import").join(format!("{id}-1"));
    std::fs::create_dir_all(&private).unwrap();
    std::fs::hard_link(harness.staging.join(&one), private.join("One.flac")).unwrap();
    (id, one, private)
}

fn hold_cleanup(harness: &mut Harness) -> tokio::sync::mpsc::UnboundedReceiver<()> {
    use crate::features::acquisition::service::{FinishBoundary, FinishHook};
    let (hold, entered) = Hold::new();
    let once = Arc::new(Mutex::new(Some(hold)));
    let hook: FinishHook = Arc::new(move |boundary| {
        let hold = if boundary == FinishBoundary::IntentDurable {
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

#[sqlx::test]
async fn failed_legacy_adoption_resumes_cleanup_before_a_successor_start(pool: PgPool) {
    let (mut harness, _) = race(pool, 1).await;
    let (id, one, private) = legacy_failed(&harness).await;
    let mut cleaning = hold_cleanup(&mut harness);
    let run = advance(&harness, id);
    entered(&mut cleaning).await;
    assert!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .is_some()
    );
    let intent = attempt(&harness, id, 1).await.finish_intent.unwrap();
    assert_eq!(intent.update.reason.as_deref(), Some("import_skipped"));
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec![one.clone()]
    );
    assert!(harness.staging.join(&one).exists());
    assert!(private.join("One.flac").exists());
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
    abort(run).await;
    harness.restart();
    starts(&harness, &["b"]);
    silent(&harness, 1);
    let (hold, mut starting) = Hold::new();
    *harness.transfer.start_hold.lock().unwrap() = Some(Arc::clone(&hold));
    let run = advance(&harness, id);
    entered(&mut starting).await;
    assert!(!harness.staging.join(one).exists());
    assert!(!private.exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert!(attempt(&harness, id, 1).await.finish_intent.is_none());
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
    hold.release(1);
    assert_eq!(
        joined(run).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 1);
}

#[sqlx::test]
async fn failed_legacy_adoption_rolls_back_round_and_intent_together(pool: PgPool) {
    let (harness, _) = race(pool.clone(), 1).await;
    let (id, one, private) = legacy_failed(&harness).await;
    sqlx::raw_sql(
        "CREATE FUNCTION reject_legacy_intent() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN IF NEW.finish_intent IS NOT NULL THEN RAISE EXCEPTION 'held adoption failure'; \
         END IF; RETURN NEW; END $$; \
         CREATE TRIGGER reject_legacy_intent BEFORE UPDATE ON acquisition_attempts \
         FOR EACH ROW EXECUTE FUNCTION reject_legacy_intent()",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Repository(_))
    ));
    assert!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .is_none()
    );
    assert!(attempt(&harness, id, 1).await.finish_intent.is_none());
    assert_eq!(
        harness.store.claimed_paths(id, 1).await.unwrap(),
        vec![one.clone()]
    );
    assert!(harness.staging.join(&one).exists());
    assert!(private.join("One.flac").exists());
    assert!(harness.transfer.started_keys.lock().unwrap().is_empty());
    sqlx::raw_sql("DROP TRIGGER reject_legacy_intent ON acquisition_attempts; DROP FUNCTION reject_legacy_intent()")
        .execute(&pool).await.unwrap();
    starts(&harness, &["b"]);
    silent(&harness, 1);
    assert_eq!(
        held_poll(&harness, id, 1).await,
        Step::Wait(Duration::from_secs(10))
    );
    assert!(!harness.staging.join(one).exists());
    assert!(!private.exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
}

#[sqlx::test]
async fn a_legacy_failed_attempt_cleans_before_the_next_round(pool: PgPool) {
    let (harness, _) = race(pool, 1).await;
    let (id, one, _) = legacy_failed(&harness).await;
    starts(&harness, &["b"]);
    silent(&harness, 1);
    assert_eq!(
        held_poll(&harness, id, 1).await,
        Step::Wait(Duration::from_secs(10))
    );
    assert!(!harness.staging.join(one).exists());
    assert!(harness.store.claimed_paths(id, 1).await.unwrap().is_empty());
    assert!(attempt(&harness, id, 1).await.finish_intent.is_none());
    assert_eq!(
        harness
            .store
            .find_request(id)
            .await
            .unwrap()
            .unwrap()
            .round
            .unwrap()
            .attempts,
        vec![2]
    );
}

#[sqlx::test]
async fn first_bytes_after_the_start_deadline_cannot_win(pool: PgPool) {
    let (harness, now) = race(pool, 2).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a", "b", "c"]);
    silent(&harness, 2);
    assert_eq!(
        held_poll(&harness, id, 2).await,
        Step::Wait(Duration::from_secs(10))
    );
    let path = staging_file(&harness, "B/b.flac");
    *now.lock().unwrap() += chrono::Duration::seconds(601);
    statuses(
        &harness,
        &[
            bytes(),
            progress_with(TransferPhase::ReportedComplete, Some(&path)),
            progress(TransferPhase::Queued),
        ],
    );
    let (hold, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("a".to_owned(), Arc::clone(&hold)));
    let run = advance(&harness, id);
    entered(&mut cancelling).await;
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(round.winner, None);
    assert_eq!(round.won_at, None);
    for number in [1, 2] {
        let intent = attempt(&harness, id, number).await.finish_intent.unwrap();
        assert_eq!(intent.update.reason.as_deref(), Some("start_timeout"));
        assert!(intent.cancel);
    }
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 2);
    assert!(harness.store.load_attempt(id, 3).await.unwrap().is_none());
    hold.release(1);
    assert_eq!(
        joined(run).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    for number in [1, 2] {
        let attempt = attempt(&harness, id, number).await;
        assert_eq!(attempt.reason.as_deref(), Some("start_timeout"));
        assert!(attempt.finish_intent.is_none());
        assert!(
            harness
                .store
                .claimed_paths(id, number)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(!harness.staging.join(path).exists());
    assert!(harness.importer.seen.lock().unwrap().is_empty());
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 3);
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(round.attempts, vec![3]);
    assert_eq!(round.winner, None);
    assert_eq!(round.won_at, None);
}

#[sqlx::test]
async fn a_poll_crossing_the_start_deadline_cannot_elect_a_winner(pool: PgPool) {
    let (harness, now) = race(pool, 2).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a", "b", "c"]);
    silent(&harness, 2);
    assert_eq!(
        held_poll(&harness, id, 2).await,
        Step::Wait(Duration::from_secs(10))
    );
    statuses(
        &harness,
        &[
            progress(TransferPhase::Active),
            progress(TransferPhase::Queued),
            progress(TransferPhase::Queued),
        ],
    );
    let (hold, mut observing) = Hold::new();
    *harness.transfer.status_hold.lock().unwrap() = Some(Arc::clone(&hold));
    let run = advance(&harness, id);
    entered(&mut observing).await;
    *now.lock().unwrap() += chrono::Duration::seconds(601);
    hold.release(1);
    assert_eq!(
        joined(run).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    for number in [1, 2] {
        assert_eq!(
            attempt(&harness, id, number).await.reason.as_deref(),
            Some("start_timeout")
        );
    }
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(round.attempts, vec![3]);
    assert_eq!(round.winner, None);
    assert_eq!(round.won_at, None);
    assert!(harness.importer.seen.lock().unwrap().is_empty());
}

#[sqlx::test]
async fn an_expired_incomplete_start_recovers_only_for_timeout_cleanup(pool: PgPool) {
    let (mut harness, now) = race(pool, 2).await;
    let id = request(&harness, &["a", "b", "c"]).await;
    starts(&harness, &["a"]);
    harness
        .transfer
        .starts
        .lock()
        .unwrap()
        .push_back(Err(TransferError::Unavailable));
    assert!(matches!(
        joined(advance(&harness, id)).await,
        Err(AcquisitionError::Unavailable)
    ));
    let initial = harness.transfer.started_keys.lock().unwrap().clone();
    let a = staging_file(&harness, "A/a.flac");
    let b = staging_file(&harness, "B/b.flac");
    *now.lock().unwrap() += chrono::Duration::seconds(601);
    harness.restart();
    let mut cleaning = hold_cleanup(&mut harness);
    statuses(
        &harness,
        &[
            progress_with(TransferPhase::ReportedComplete, Some(&a)),
            progress_with(TransferPhase::ReportedComplete, Some(&b)),
            progress(TransferPhase::Queued),
        ],
    );
    let run = advance(&harness, id);
    entered(&mut cleaning).await;
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(round.winner, None);
    assert_eq!(round.won_at, None);
    for number in [1, 2] {
        assert_eq!(
            attempt(&harness, id, number)
                .await
                .finish_intent
                .unwrap()
                .update
                .reason
                .as_deref(),
            Some("start_timeout")
        );
    }
    assert!(
        attempt(&harness, id, 2)
            .await
            .finish_intent
            .unwrap()
            .recover_start
    );
    assert_eq!(*harness.transfer.started_keys.lock().unwrap(), initial);
    abort(run).await;
    harness.restart();
    starts(&harness, &["b", "c"]);
    let (hold, mut cancelling) = Hold::new();
    *harness.transfer.cancel_hold.lock().unwrap() = Some(("b".to_owned(), hold));
    let run = advance(&harness, id);
    entered(&mut cancelling).await;
    assert_eq!(
        attempt(&harness, id, 1).await.reason.as_deref(),
        Some("start_timeout")
    );
    let recovered = attempt(&harness, id, 2).await.finish_intent.unwrap();
    assert_eq!(recovered.update.reason.as_deref(), Some("start_timeout"));
    assert!(!recovered.recover_start);
    assert_eq!(recovered.update.handles, vec![Some("b".to_owned())]);
    assert_eq!(recovered.update.delivered, vec![PathBuf::from(&b)]);
    assert_eq!(
        harness.store.claimed_paths(id, 2).await.unwrap(),
        vec![b.clone()]
    );
    assert!(!harness.staging.join(&a).exists());
    assert!(harness.staging.join(&b).exists());
    let resumed = harness.transfer.started_keys.lock().unwrap().clone();
    assert_eq!(resumed.len(), 3);
    assert_eq!(resumed[1], resumed[2]);
    assert!(harness.store.load_attempt(id, 3).await.unwrap().is_none());
    assert!(harness.importer.seen.lock().unwrap().is_empty());
    abort(run).await;
    harness.restart();
    let (hold, mut starting) = Hold::new();
    *harness.transfer.start_hold.lock().unwrap() = Some(Arc::clone(&hold));
    let run = advance(&harness, id);
    entered(&mut starting).await;
    assert!(!harness.staging.join(b).exists());
    for number in [1, 2] {
        assert_eq!(
            attempt(&harness, id, number).await.reason.as_deref(),
            Some("start_timeout")
        );
        assert!(attempt(&harness, id, number).await.finish_intent.is_none());
        assert!(
            harness
                .store
                .claimed_paths(id, number)
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert_eq!(*harness.transfer.started_keys.lock().unwrap(), resumed);
    hold.release(1);
    assert_eq!(
        joined(run).await.unwrap(),
        Step::Wait(Duration::from_secs(10))
    );
    let round = harness
        .store
        .find_request(id)
        .await
        .unwrap()
        .unwrap()
        .round
        .unwrap();
    assert_eq!(round.attempts, vec![3]);
    assert_eq!(round.winner, None);
    assert_eq!(round.won_at, None);
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 4);
}

#[sqlx::test]
async fn no_untried_users_is_exhausted(pool: PgPool) {
    let (harness, now) = race(pool, 2).await;
    let id = request(&harness, &["a", "b"]).await;
    starts(&harness, &["a", "b"]);
    silent(&harness, 2);
    assert_eq!(
        held_poll(&harness, id, 2).await,
        Step::Wait(Duration::from_secs(10))
    );
    *now.lock().unwrap() += chrono::Duration::seconds(600);
    silent(&harness, 2);
    assert_eq!(joined(advance(&harness, id)).await.unwrap(), Step::Done);
    assert_eq!(state(&harness, id).await, RequestState::Exhausted);
    for number in [1, 2] {
        assert_eq!(
            attempt(&harness, id, number).await.reason.as_deref(),
            Some("start_timeout")
        );
    }
    assert_eq!(harness.transfer.started_keys.lock().unwrap().len(), 2);
    assert_eq!(harness.transfer.cancelled.lock().unwrap().len(), 2);
}
