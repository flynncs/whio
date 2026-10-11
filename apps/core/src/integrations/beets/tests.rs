use std::{
    ffi::CString,
    fs,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::features::acquisition::ports::{
    AcquisitionTarget, ImportError, ImportJob, ImportOutcome, Importer, MbId,
};

use crate::platform::timer::Timer;
use crate::platform::timer::manual::ManualTimer;

use super::{Beets, config};

const RELEASE: &str = "11111111-1111-1111-1111-111111111111";
const RECORDING: &str = "30000000-0000-0000-0000-000000000003";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("whio-beets-test-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(&root).unwrap();

        Self { root }
    }

    fn file(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, b"").unwrap();

        path
    }

    fn fifo(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();

        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);

        path
    }

    fn install(&self, command: &str) -> String {
        let Some(source) = fixture_source(command) else {
            return command.to_owned();
        };
        let target = self.root.join(command);
        fs::write(&target, source).unwrap();
        let mut permissions = fs::metadata(&target).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&target, permissions).unwrap();

        target.display().to_string()
    }

    fn beets(
        &self,
        script: &str,
        deadline: Duration,
        stall: Duration,
        timer: Arc<dyn Timer>,
    ) -> Beets {
        Beets::with_work_root(
            &config::Config {
                command: self.install(script),
                config: PathBuf::from("/etc/whio/beets.yaml"),
                deadline,
                stall,
            },
            self.root.join("library"),
            self.root.clone(),
            timer,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixture_source(name: &str) -> Option<&'static str> {
    match name {
        "album.sh" => Some(include_str!("../../../tests/fixtures/beets/album.sh")),
        "singleton.sh" => Some(include_str!("../../../tests/fixtures/beets/singleton.sh")),
        "empty.sh" => Some(include_str!("../../../tests/fixtures/beets/empty.sh")),
        "fail.sh" => Some(include_str!("../../../tests/fixtures/beets/fail.sh")),
        "stall.sh" => Some(include_str!("../../../tests/fixtures/beets/stall.sh")),
        "child_stall.sh" => Some(include_str!("../../../tests/fixtures/beets/child_stall.sh")),
        "descendant_holds.sh" => Some(include_str!(
            "../../../tests/fixtures/beets/descendant_holds.sh"
        )),
        "cancel.sh" => Some(include_str!("../../../tests/fixtures/beets/cancel.sh")),
        "partial.sh" => Some(include_str!("../../../tests/fixtures/beets/partial.sh")),
        "flood.sh" => Some(include_str!("../../../tests/fixtures/beets/flood.sh")),
        "listing_stall.sh" => Some(include_str!(
            "../../../tests/fixtures/beets/listing_stall.sh"
        )),
        "sentinel.sh" => Some(include_str!("../../../tests/fixtures/beets/sentinel.sh")),
        _ => None,
    }
}

fn release_job(path: &Path) -> ImportJob {
    ImportJob {
        target: AcquisitionTarget::Release(MbId::parse(RELEASE).unwrap()),
        path: path.to_path_buf(),
    }
}

fn recording_job(path: &Path) -> ImportJob {
    ImportJob {
        target: AcquisitionTarget::Recording(MbId::parse(RECORDING).unwrap()),
        path: path.to_path_buf(),
    }
}

fn args(path: &Path) -> String {
    fs::read_to_string(format!("{}.args", path.display())).unwrap()
}

async fn await_event(path: &Path) {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path)
        .await
        .expect("the event fifo opens");
    let mut byte = [0u8; 1];
    let read = file.read(&mut byte).await.expect("the event is written");

    assert_eq!(read, 1, "the stand-in signalled readiness");
}

fn event(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}.{suffix}", path.display()))
}

#[tokio::test]
async fn the_album_command_pins_the_release() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "album.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    beets.import(&release_job(&path)).await.unwrap();

    let args = args(&path);
    assert!(args.contains("-c /etc/whio/beets.yaml"));
    assert!(args.contains(&format!("-d {}", fixture.root.join("library").display())));
    assert!(args.contains("import -q --search-id"));
    assert!(args.contains(&format!("--search-id {RELEASE}")));
    assert!(args.contains(&path.display().to_string()));
    assert!(!args.contains(" -s "));
    assert!(args.contains("-l ") && args.contains("/library.db"));
}

#[tokio::test]
async fn a_singleton_uses_s() {
    let fixture = Fixture::new();
    let path = fixture.file("single.flac");
    let beets = fixture.beets(
        "singleton.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    beets.import(&recording_job(&path)).await.unwrap();

    assert!(
        args(&path).contains(&format!("import -q -s --search-id {RECORDING}")),
        "a track request imports a singleton"
    );
}

#[tokio::test]
async fn listed_items_are_parsed() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "album.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    let ImportOutcome { items } = beets.import(&release_job(&path)).await.unwrap();

    assert_eq!(items.len(), 2);
    assert_eq!(items[0].release_id.as_deref(), Some(RELEASE));
    assert_eq!(
        items[0].recording_id.as_deref(),
        Some("30000000-0000-0000-0000-000000000001")
    );
    assert_eq!(items[0].path, PathBuf::from("/library/a/01.flac"));
}

#[tokio::test]
async fn an_empty_listing_is_skipped() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "empty.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    let ImportOutcome { items } = beets.import(&release_job(&path)).await.unwrap();

    assert!(items.is_empty());
}

#[tokio::test]
async fn an_empty_id_field_is_none() {
    let fixture = Fixture::new();
    let path = fixture.file("single.flac");
    let beets = fixture.beets(
        "singleton.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    let ImportOutcome { items } = beets.import(&recording_job(&path)).await.unwrap();

    assert_eq!(items.len(), 1);
    assert_eq!(items[0].release_id, None);
    assert_eq!(items[0].recording_id.as_deref(), Some(RECORDING));
}

#[tokio::test]
async fn silence_past_the_stall_window_kills_the_group() {
    let fixture = Fixture::new();
    let path = fixture.fifo("stall.fifo");
    let ready = fixture.fifo("stall.fifo.ready");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "stall.sh",
        Duration::from_secs(3600),
        Duration::from_secs(300),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;
    timer.advance(Duration::from_secs(301));

    assert_eq!(import.await.unwrap().unwrap_err(), ImportError::Stalled);
}

#[tokio::test]
async fn the_deadline_kills_the_group() {
    let fixture = Fixture::new();
    let path = fixture.fifo("deadline.fifo");
    let ready = fixture.fifo("deadline.fifo.ready");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "stall.sh",
        Duration::from_secs(600),
        Duration::from_secs(3600),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;
    timer.advance(Duration::from_secs(601));

    assert_eq!(import.await.unwrap().unwrap_err(), ImportError::TimedOut);
}

#[tokio::test]
async fn a_child_of_beets_is_killed_too() {
    let fixture = Fixture::new();
    let path = fixture.fifo("child.fifo");
    let ready = fixture.fifo("child.fifo.ready");
    let _hold = fixture.fifo("child.fifo.hold");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "child_stall.sh",
        Duration::from_secs(3600),
        Duration::from_secs(300),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;

    timer.await_deadline(Duration::from_secs(300)).await;
    timer.advance(Duration::from_secs(301));

    let result = tokio::time::timeout(Duration::from_secs(10), import)
        .await
        .expect("a blocking descendant is killed with the group");
    assert_eq!(result.unwrap().unwrap_err(), ImportError::Stalled);
}

#[tokio::test]
async fn a_missing_binary_is_unavailable() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "/nonexistent/beet-does-not-exist",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    assert_eq!(
        beets.import(&release_job(&path)).await.unwrap_err(),
        ImportError::Unavailable
    );
}

#[tokio::test]
async fn a_non_zero_exit_is_failed() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "fail.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    assert_eq!(
        beets.import(&release_job(&path)).await.unwrap_err(),
        ImportError::Failed { status: Some(3) }
    );
}

#[tokio::test]
async fn the_temporary_library_is_removed() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "album.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );

    beets.import(&release_job(&path)).await.unwrap();

    let leftovers = fs::read_dir(&fixture.root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("whio-beets-")
        })
        .count();

    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn a_descendant_holding_the_pipe_does_not_hang_the_run() {
    let fixture = Fixture::new();
    let path = fixture.fifo("hold.fifo");
    let ready = fixture.fifo("hold.fifo.ready");
    let _hold = fixture.fifo("hold.fifo.hold");
    let beets = fixture.beets(
        "descendant_holds.sh",
        Duration::from_secs(3600),
        Duration::from_secs(3600),
        ManualTimer::new(),
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;

    let result = tokio::time::timeout(Duration::from_secs(10), import)
        .await
        .expect("an in-group descendant holding stdout must not hang the run");
    assert!(
        result.unwrap().is_ok(),
        "the listing runs after the group kill"
    );
}

#[tokio::test]
async fn the_listing_step_runs_under_the_import_deadline() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let listing = fixture.fifo("track.flac.listing");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "listing_stall.sh",
        Duration::from_secs(600),
        Duration::from_secs(3600),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&listing).await;
    timer.advance(Duration::from_secs(601));

    assert_eq!(import.await.unwrap().unwrap_err(), ImportError::TimedOut);
}

#[tokio::test]
async fn partial_line_output_counts_as_activity() {
    use tokio::io::AsyncWriteExt;

    let (mut writer, reader) = tokio::io::duplex(64);
    let timer = ManualTimer::new();
    let (tx, mut activity) = tokio::sync::watch::channel(Duration::ZERO);
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let handle = tokio::spawn(super::read_stream(reader, None, tx, reader_timer, None));

    timer.advance(Duration::from_secs(5));
    writer.write_all(b"partial-without-newline").await.unwrap();
    activity.changed().await.unwrap();

    assert!(
        *activity.borrow() >= Duration::from_secs(5),
        "partial bytes without a newline count as activity"
    );

    drop(writer);
    handle.await.unwrap().unwrap();
}

#[derive(Clone, Default)]
struct Capture(Arc<parking_lot::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buffer);

        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn the_child_output_is_logged_only_as_a_byte_count() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "sentinel.sh",
        Duration::from_secs(10),
        Duration::from_secs(10),
        ManualTimer::new(),
    );
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let beets = beets.with_dispatch(dispatch);

    beets.import(&release_job(&path)).await.unwrap();

    let logs = String::from_utf8_lossy(&capture.0.lock()).into_owned();
    assert!(!logs.contains("SUPER_SECRET"), "no raw child output");
    assert!(!logs.contains("/etc/passwd"), "no raw paths");
    assert!(!logs.contains("hunter2"), "no raw credentials");
    assert!(
        logs.contains("beets output read"),
        "a validated event remains"
    );
}

#[tokio::test]
async fn a_permission_denied_binary_is_failed() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let command = fixture.root.join("not-executable.sh");
    fs::write(&command, fixture_source("album.sh").unwrap()).unwrap();
    let mut permissions = fs::metadata(&command).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o644);
    fs::set_permissions(&command, permissions).unwrap();
    let beets = Beets::with_work_root(
        &config::Config {
            command: command.display().to_string(),
            config: PathBuf::from("/etc/whio/beets.yaml"),
            deadline: Duration::from_secs(10),
            stall: Duration::from_secs(10),
        },
        fixture.root.join("library"),
        fixture.root.clone(),
        ManualTimer::new(),
    );

    assert_eq!(
        beets.import(&release_job(&path)).await.unwrap_err(),
        ImportError::Failed { status: None }
    );
}

#[tokio::test]
async fn live_partial_bytes_keep_the_run_alive() {
    let fixture = Fixture::new();
    let path = fixture.fifo("partial.fifo");
    let ready = fixture.fifo("partial.fifo.ready");
    let partial = fixture.fifo("partial.fifo.partial");
    let control = event(&path, "control");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "partial.sh",
        Duration::from_secs(3600),
        Duration::from_secs(1000),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let (activity, mut observed_activity) = tokio::sync::watch::channel(Duration::ZERO);
    let beets = beets.with_activity(activity);
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;
    timer.await_deadline(Duration::from_secs(1000)).await;
    timer.advance(Duration::from_secs(900));
    tokio::fs::write(&control, b"go\n").await.unwrap();
    await_event(&partial).await;

    tokio::time::timeout(Duration::from_secs(5), observed_activity.changed())
        .await
        .expect("the parent reader acknowledges the partial bytes")
        .unwrap();
    assert_eq!(*observed_activity.borrow(), Duration::from_secs(900));
    timer.await_deadline(Duration::from_secs(1900)).await;
    assert_eq!(
        timer.waiting(),
        2,
        "only the deadline and rearmed stall wait remain"
    );

    timer.advance(Duration::from_secs(101));
    assert!(
        !import.is_finished(),
        "live partial bytes, not newlines, keep the run alive"
    );

    timer.advance(Duration::from_secs(900));
    assert_eq!(import.await.unwrap().unwrap_err(), ImportError::Stalled);
}

#[tokio::test]
async fn output_larger_than_the_pipe_does_not_block() {
    let fixture = Fixture::new();
    let path = fixture.file("track.flac");
    let beets = fixture.beets(
        "flood.sh",
        Duration::from_secs(3600),
        Duration::from_secs(3600),
        ManualTimer::new(),
    );
    let job = release_job(&path);

    let result = tokio::time::timeout(Duration::from_secs(10), beets.import(&job))
        .await
        .expect("readers drain a child that exceeds the pipe capacity");

    assert!(result.is_ok(), "a verbose child completes");
}

#[tokio::test]
async fn cancelling_the_import_kills_the_group_and_removes_scratch() {
    let fixture = Fixture::new();
    let path = fixture.fifo("cancel.fifo");
    let ready = fixture.fifo("cancel.fifo.ready");
    let _hold = fixture.fifo("cancel.fifo.hold");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "cancel.sh",
        Duration::from_secs(3600),
        Duration::from_secs(3600),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let job = release_job(&path);
    let import = tokio::spawn(async move { beets.import(&job).await });

    await_event(&ready).await;

    timer.await_waiter().await;
    import.abort();
    assert!(import.await.unwrap_err().is_cancelled());

    let leftovers = std::fs::read_dir(&fixture.root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("whio-beets-")
        })
        .count();
    assert_eq!(leftovers, 0, "scratch is removed on cancellation");
}

#[test]
fn an_old_watchdog_does_not_stall_recent_activity() {
    let stall = Duration::from_secs(300);
    let last = Duration::from_secs(150);

    assert!(!super::stall_elapsed(Duration::from_secs(300), last, stall));
    assert!(!super::stall_elapsed(Duration::from_secs(449), last, stall));
    assert!(super::stall_elapsed(Duration::from_secs(450), last, stall));
    assert!(super::stall_elapsed(Duration::from_secs(451), last, stall));
}

struct DropSignal(std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.lock().unwrap().take() {
            let _ = sender.send(());
        }
    }
}

fn pending_reader() -> (
    impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let signal = DropSignal(std::sync::Mutex::new(Some(sender)));
    let reader = async move {
        let _signal = signal;
        std::future::pending::<()>().await;

        Ok(())
    };

    (reader, receiver)
}

struct FailingReader;

impl tokio::io::AsyncRead for FailingReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buffer: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Err(std::io::Error::other("reader failed")))
    }
}

#[tokio::test]
async fn dropping_readers_aborts_unfinished_readers() {
    let (reader, signal) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);

    drop(readers);

    tokio::time::timeout(Duration::from_secs(5), signal)
        .await
        .expect("the reader is aborted when its owner is dropped")
        .unwrap();
}

#[tokio::test]
async fn a_drain_past_its_deadline_aborts_readers() {
    let timer = ManualTimer::new();
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let (reader, signal) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);

    let drain = readers.drain(&reader_timer, Duration::from_secs(10));
    let advance = async {
        timer.await_waiter().await;
        timer.advance(Duration::from_secs(11));
    };
    let (result, ()) = tokio::join!(drain, advance);

    assert!(
        result.is_err(),
        "a drain past its deadline is not a success"
    );
    assert_eq!(result, Err(ImportError::TimedOut));
    tokio::time::timeout(Duration::from_secs(5), signal)
        .await
        .expect("the unfinished reader is aborted")
        .unwrap();
}

#[tokio::test]
async fn a_reader_error_fails_the_drain() {
    let timer = ManualTimer::new();
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let (activity, _receiver) = tokio::sync::watch::channel(Duration::ZERO);
    let mut readers = super::Readers::default();
    readers.push(super::read_stream(
        FailingReader,
        None,
        activity,
        Arc::clone(&reader_timer),
        None,
    ));

    let result = readers.drain(&reader_timer, Duration::from_secs(10)).await;

    assert_eq!(result, Err(ImportError::Failed { status: None }));
}

#[tokio::test]
async fn cancelling_a_pending_drain_aborts_its_readers() {
    let timer = ManualTimer::new();
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let (reader, teardown) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);
    let drain =
        tokio::spawn(async move { readers.drain(&reader_timer, Duration::from_secs(10)).await });

    tokio::time::timeout(
        Duration::from_secs(5),
        timer.await_deadline(Duration::from_secs(10)),
    )
    .await
    .expect("the non-EOF drain is pending");
    drain.abort();
    assert!(drain.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("cancelling drain tears down its owned reader")
        .unwrap();
    assert_eq!(timer.waiting(), 0, "cancelled waits have no registration");
}

#[tokio::test]
async fn a_stalled_run_with_a_non_eof_drain_expires_as_timed_out() {
    let timer = ManualTimer::new();
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let (reader, teardown) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);
    let finish = super::finish_run(
        Err(ImportError::Stalled),
        &mut readers,
        &reader_timer,
        Duration::from_secs(10),
    );
    let expire = async {
        timer.await_deadline(Duration::from_secs(10)).await;
        timer.advance(Duration::from_secs(11));
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(finish, expire)
    })
    .await
    .expect("the stalled run's drain is bounded");

    assert_eq!(result.unwrap_err(), ImportError::TimedOut);
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("the non-EOF reader acknowledges teardown")
        .unwrap();
    assert_eq!(timer.waiting(), 0);
}

#[tokio::test]
async fn cancelling_scratch_creation_cleans_after_creation_finishes() {
    let fixture = Fixture::new();
    let path = fixture.root.join("whio-beets-held-creation");
    let (started_tx, started) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let (cleanup_ack, cleaned) = tokio::sync::oneshot::channel();
    let mut scratch = super::Scratch::new(path.clone());
    scratch.cleanup_ack = Some(cleanup_ack);
    let creation = tokio::spawn(scratch.create(move |path| {
        started_tx.send(()).unwrap();
        held.recv().unwrap();
        std::fs::create_dir_all(path)
    }));

    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .expect("creation is held in flight")
        .unwrap();
    creation.abort();
    assert!(matches!(creation.await, Err(error) if error.is_cancelled()));
    assert!(!path.exists());
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(5), cleaned)
        .await
        .expect("cleanup runs after the in-flight creation")
        .unwrap();
    assert!(!path.exists(), "no directory survives cancelled creation");
}

#[tokio::test]
async fn a_reader_error_is_not_hidden_by_another_non_eof_reader() {
    let timer = ManualTimer::new() as Arc<dyn Timer>;
    let (reader, teardown) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);
    readers.push(async { Err(std::io::Error::other("reader failed")) });

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        readers.drain(&timer, Duration::from_secs(10)),
    )
    .await
    .expect("a reader failure does not wait for another reader's EOF");
    assert_eq!(result, Err(ImportError::Failed { status: None }));
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("the other reader is torn down")
        .unwrap();
}

#[tokio::test]
async fn a_stalled_run_with_a_reader_failure_is_failed() {
    let timer = ManualTimer::new() as Arc<dyn Timer>;
    let mut readers = super::Readers::default();
    readers.push(async { Err(std::io::Error::other("reader failed")) });
    let result = super::finish_run(
        Err(ImportError::Stalled),
        &mut readers,
        &timer,
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(result.unwrap_err(), ImportError::Failed { status: None });
}

#[tokio::test]
async fn a_wait_failure_remains_failed_when_its_non_eof_drain_expires() {
    let timer = ManualTimer::new();
    let reader_timer = Arc::clone(&timer) as Arc<dyn Timer>;
    let (reader, teardown) = pending_reader();
    let mut readers = super::Readers::default();
    readers.push(reader);
    let finish = super::finish_run(
        Err(ImportError::Failed { status: None }),
        &mut readers,
        &reader_timer,
        Duration::from_secs(10),
    );
    let expire = async {
        timer.await_deadline(Duration::from_secs(10)).await;
        timer.advance(Duration::from_secs(11));
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(finish, expire)
    })
    .await
    .expect("a failed wait still tears down its reader");

    assert_eq!(result.unwrap_err(), ImportError::Failed { status: None });
    tokio::time::timeout(Duration::from_secs(5), teardown)
        .await
        .expect("the reader acknowledges teardown")
        .unwrap();
}

#[tokio::test]
async fn simultaneous_activity_and_an_old_watchdog_rearm_supervision() {
    let fixture = Fixture::new();
    let path = fixture.fifo("old-watchdog.fifo");
    let ready = fixture.fifo("old-watchdog.fifo.ready");
    let timer = ManualTimer::new();
    let beets = fixture.beets(
        "stall.sh",
        Duration::from_secs(3600),
        Duration::from_secs(300),
        Arc::clone(&timer) as Arc<dyn Timer>,
    );
    let mut command = tokio::process::Command::new(&beets.command);
    command.arg(&path).process_group(0);
    let mut child = command.spawn().unwrap();
    let group = super::ProcessGroup::of(&child);
    tokio::time::timeout(Duration::from_secs(5), await_event(&ready))
        .await
        .expect("the child is held");
    let (activity, mut observed) = tokio::sync::watch::channel(Duration::ZERO);
    let supervision = beets.supervise(&mut child, &group, &mut observed, Duration::from_secs(3600));
    tokio::pin!(supervision);

    std::future::poll_fn(|cx| {
        use std::future::Future;
        assert!(supervision.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    assert_eq!(timer.waiting(), 2);
    timer.advance(Duration::from_secs(150));
    activity.send_replace(timer.now());
    timer.advance(Duration::from_secs(150));
    std::future::poll_fn(|cx| {
        use std::future::Future;
        assert!(supervision.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        timer.await_deadline(Duration::from_secs(450)),
    )
    .await
    .expect("the ready old watchdog rechecks and rearms from recent activity");
    assert_eq!(timer.waiting(), 2);

    timer.advance(Duration::from_secs(151));
    let result = tokio::time::timeout(Duration::from_secs(5), supervision)
        .await
        .expect("only the rearmed watchdog stalls the child");
    assert_eq!(result.unwrap_err(), ImportError::Stalled);
    assert_eq!(timer.waiting(), 0);
}
