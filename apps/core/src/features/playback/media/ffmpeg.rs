use std::{path::PathBuf, process::Stdio, sync::Arc};

use async_trait::async_trait;
use bytes::Bytes;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc},
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use tracing::Instrument;

use crate::features::playback::{AacMode, DeliveryError, Representation};

use super::MediaFetchError;
use super::ports::{ConversionByteStream, ConversionInput, ConversionTarget, Converter, MediaBody};

pub const DEFAULT_FFMPEG_MAX_CONVERSIONS: usize = 4;
const READ_CHUNK_BYTES: usize = 64 * 1024;
const STDERR_TAIL_BYTES: usize = 4 * 1024;
const OUTPUT_QUEUE_CHUNKS: usize = 4;
const REDACTED: &str = "<redacted>";

pub struct FfmpegConverter {
    binary: PathBuf,
    available: bool,
    conversions: Arc<Semaphore>,
    #[cfg(test)]
    stdout_eof: Option<Arc<Notify>>,
}

impl FfmpegConverter {
    pub async fn probe(binary: PathBuf, max_conversions: usize) -> Self {
        let available = Command::new(&binary)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|status| status.success())
            .unwrap_or(false);
        if !available {
            tracing::warn!("ffmpeg is not available; converted playback will fail");
        }

        Self {
            binary,
            available,
            conversions: Arc::new(Semaphore::new(max_conversions.max(1))),
            #[cfg(test)]
            stdout_eof: None,
        }
    }
}

#[async_trait]
impl Converter for FfmpegConverter {
    fn available(&self) -> bool {
        self.available
    }

    #[tracing::instrument(skip_all, fields(representation = ?target.representation))]
    async fn start(
        &self,
        input: ConversionInput,
        target: ConversionTarget,
    ) -> Result<MediaBody, DeliveryError> {
        if !self.available {
            return Err(DeliveryError::ConversionUnavailable);
        }
        let permit = self
            .conversions
            .clone()
            .try_acquire_owned()
            .map_err(|_| DeliveryError::Overloaded)?;

        let mut child = match self.spawn(&input, target) {
            Ok(child) => child,
            Err(error) => {
                log_failure(target.representation, None, &error.to_string());
                return Err(DeliveryError::Conversion);
            }
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");

        let (sender, receiver) = mpsc::channel(OUTPUT_QUEUE_CHUNKS);
        let cancel = Arc::new(Notify::new());
        let representation = target.representation;
        let feeder_input = match input {
            ConversionInput::Stream(stream) => Some(stream),
            ConversionInput::Path(_) => None,
        };
        let span =
            tracing::info_span!("playback_conversion_body", representation = ?representation);
        tokio::spawn(
            supervise(ConversionProcess {
                child,
                stdin,
                input: feeder_input,
                stdout,
                stderr,
                sender,
                cancel: Arc::clone(&cancel),
                representation,
                #[cfg(test)]
                stdout_eof: self.stdout_eof.clone(),
                _permit: permit,
            })
            .instrument(span),
        );

        Ok(MediaBody {
            content_length: None,
            chunks: Box::pin(ConversionStream {
                inner: ReceiverStream::new(receiver),
                cancel,
            }),
        })
    }
}

impl FfmpegConverter {
    fn spawn(&self, input: &ConversionInput, target: ConversionTarget) -> std::io::Result<Child> {
        let mut command = Command::new(&self.binary);
        command.args(["-nostdin", "-v", "error", "-i"]);
        match input {
            ConversionInput::Stream(_) => {
                command.arg("pipe:0");
            }
            ConversionInput::Path(path) => {
                command.arg(path);
            }
        }
        command.args(["-map", "0:a:0", "-vn", "-c:a"]);
        match target.representation {
            Representation::Aac(AacMode::Copy) => {
                command.arg("copy");
            }
            Representation::Aac(AacMode::Encode { bitrate_kbps }) => {
                command.arg("aac");
                command.arg("-b:a").arg(format!("{bitrate_kbps}k"));
            }
            Representation::Original => {
                unreachable!("the converter only receives converted representations")
            }
        }
        command.args(["-f", "adts", "pipe:1"]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        command.spawn()
    }
}

struct ConversionStream {
    inner: ReceiverStream<Result<Bytes, MediaFetchError>>,
    cancel: Arc<Notify>,
}

impl Drop for ConversionStream {
    fn drop(&mut self) {
        self.cancel.notify_one();
    }
}

impl tokio_stream::Stream for ConversionStream {
    type Item = Result<Bytes, MediaFetchError>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.get_mut().inner).poll_next(cx)
    }
}

struct ConversionProcess {
    child: Child,
    stdin: ChildStdin,
    input: Option<ConversionByteStream>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    sender: mpsc::Sender<Result<Bytes, MediaFetchError>>,
    cancel: Arc<Notify>,
    representation: Representation,
    #[cfg(test)]
    stdout_eof: Option<Arc<Notify>>,
    _permit: OwnedSemaphorePermit,
}

async fn supervise(process: ConversionProcess) {
    let ConversionProcess {
        mut child,
        stdin,
        input,
        mut stdout,
        stderr,
        sender,
        cancel,
        representation,
        #[cfg(test)]
        stdout_eof,
        _permit,
    } = process;

    let stderr_task = tokio::spawn(drain_stderr(stderr));
    let (feeder_updates, mut feeder_events) = mpsc::channel(1);
    let feeder = tokio::spawn(async move {
        let result = feed_stdin(stdin, input).await;
        let _ = feeder_updates.send(result).await;
    });

    let mut feeder_error: Option<FeederError> = None;
    let mut read_error: Option<std::io::Error> = None;
    let mut abandoned = false;
    let mut stdout_done = false;
    let mut feeder_done = false;
    let mut status: Option<std::io::Result<std::process::ExitStatus>> = None;

    let mut buffer = vec![0u8; READ_CHUNK_BYTES];
    loop {
        if stdout_done && feeder_done && status.is_some() {
            break;
        }
        tokio::select! {
            _ = cancel.notified() => {
                abandoned = true;
                break;
            }
            event = feeder_events.recv(), if !feeder_done => {
                match event {
                    Some(Err(error)) => {
                        feeder_error = Some(error);
                        break;
                    }
                    Some(Ok(())) | None => {
                        feeder_done = true;
                    }
                }
            }
            read = stdout.read(&mut buffer), if !stdout_done => {
                match read {
                    Ok(0) => {
                        stdout_done = true;
                        #[cfg(test)]
                        if let Some(signal) = &stdout_eof {
                            signal.notify_one();
                        }
                    }
                    Ok(count) => {
                        if sender
                            .send(Ok(Bytes::copy_from_slice(&buffer[..count])))
                            .await
                            .is_err()
                        {
                            abandoned = true;
                            break;
                        }
                    }
                    Err(error) => {
                        read_error = Some(error);
                        break;
                    }
                }
            }
            result = child.wait(), if status.is_none() => {
                let failed = result
                    .as_ref()
                    .map(|status| !status.success())
                    .unwrap_or(true);
                status = Some(result);
                if failed {
                    break;
                }
            }
        }
    }

    let waited = status.is_some();
    feeder.abort();
    let _ = feeder.await;

    let child_status = match status {
        Some(result) => result,
        None => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    let stderr = stderr_task.await.unwrap_or_default();

    if abandoned {
        return;
    }

    let outcome = conversion_outcome(
        feeder_error.as_ref(),
        read_error.as_ref(),
        waited,
        &child_status,
        &stderr,
    );
    if outcome.failed {
        log_failure(representation, outcome.code, &outcome.detail);
        let _ = sender.send(Err(MediaFetchError::Conversion)).await;
    }
}

struct ConversionOutcome {
    failed: bool,
    code: Option<i32>,
    detail: String,
}

fn conversion_outcome(
    feeder_error: Option<&FeederError>,
    read_error: Option<&std::io::Error>,
    waited: bool,
    child_status: &std::io::Result<std::process::ExitStatus>,
    stderr: &StderrCapture,
) -> ConversionOutcome {
    let code = child_status.as_ref().ok().and_then(|status| status.code());
    let exited_failed = child_status
        .as_ref()
        .map(|status| !status.success())
        .unwrap_or(true);
    let killed = !waited && stopped_by_supervisor(child_status);
    let detail = if exited_failed && !killed {
        sanitize_capture(stderr)
    } else if let Some(error) = feeder_error {
        feeder_detail(error)
    } else if let Some(error) = read_error {
        sanitize(&error.to_string())
    } else {
        sanitize_capture(stderr)
    };

    ConversionOutcome {
        failed: feeder_error.is_some() || read_error.is_some() || exited_failed,
        code,
        detail,
    }
}

fn feeder_detail(error: &FeederError) -> String {
    match error {
        FeederError::Input => "source input failed".to_owned(),
        FeederError::Write(error) | FeederError::Shutdown(error) => sanitize(&error.to_string()),
    }
}

#[derive(Debug)]
enum FeederError {
    Input,
    Write(std::io::Error),
    Shutdown(std::io::Error),
}

async fn feed_stdin(
    stdin: ChildStdin,
    input: Option<ConversionByteStream>,
) -> Result<(), FeederError> {
    let Some(mut stream) = input else {
        return Ok(());
    };
    let mut stdin = stdin;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|_| FeederError::Input)?;
        stdin.write_all(&bytes).await.map_err(FeederError::Write)?;
    }
    stdin.shutdown().await.map_err(FeederError::Shutdown)
}

#[derive(Default)]
struct StderrCapture {
    text: String,
    truncated: bool,
}

async fn drain_stderr(stderr: ChildStderr) -> StderrCapture {
    let mut stderr = stderr;
    let mut collected = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 512];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                collected.extend_from_slice(&buffer[..count]);
                if collected.len() > STDERR_TAIL_BYTES {
                    let excess = collected.len() - STDERR_TAIL_BYTES;
                    collected.drain(..excess);
                    truncated = true;
                }
            }
        }
    }

    StderrCapture {
        text: String::from_utf8_lossy(&collected).trim().to_owned(),
        truncated,
    }
}

fn log_failure(representation: Representation, code: Option<i32>, detail: &str) {
    tracing::error!(
        representation = ?representation,
        code,
        detail = %detail,
        "playback conversion failed"
    );
}

fn sanitize_capture(capture: &StderrCapture) -> String {
    if capture.truncated {
        return REDACTED.to_owned();
    }
    sanitize(&capture.text)
}

#[cfg(unix)]
fn stopped_by_supervisor(status: &std::io::Result<std::process::ExitStatus>) -> bool {
    use std::os::unix::process::ExitStatusExt;

    const SIGKILL: i32 = 9;

    matches!(status, Ok(status) if status.signal() == Some(SIGKILL))
}

#[cfg(not(unix))]
fn stopped_by_supervisor(_status: &std::io::Result<std::process::ExitStatus>) -> bool {
    false
}

fn sanitize(detail: &str) -> String {
    let detail = detail.trim();
    if detail.is_empty() {
        return String::new();
    }
    if detail.contains('/') || detail.contains('\\') || detail.contains('@') {
        REDACTED.to_owned()
    } else {
        detail.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{Arc, Mutex as StdMutex, OnceLock},
        time::Duration,
    };

    use tokio_stream::StreamExt;
    use tracing::field::{Field, Visit};
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    use super::*;
    use crate::features::playback::media::ports::MediaByteStream;
    use crate::features::playback::{AacMode, Representation};

    #[derive(Clone)]
    struct LogCapture {
        entries: Arc<StdMutex<Vec<String>>>,
        recorded: Arc<Notify>,
    }

    impl LogCapture {
        fn new() -> Self {
            Self {
                entries: Arc::new(StdMutex::new(Vec::new())),
                recorded: Arc::new(Notify::new()),
            }
        }

        fn clear(&self) {
            self.entries.lock().unwrap().clear();
        }

        fn entries(&self) -> Vec<String> {
            self.entries.lock().unwrap().clone()
        }

        fn matching(&self, needle: &str) -> Vec<String> {
            self.entries()
                .into_iter()
                .filter(|entry| entry.contains(needle))
                .collect()
        }

        async fn recorded(&self, needle: &str) {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let notified = self.recorded.notified();
                    if !self.matching(needle).is_empty() {
                        return;
                    }
                    notified.await;
                }
            })
            .await
            .expect("the conversion failure must be logged");
        }
    }

    fn capture() -> &'static LogCapture {
        static CAPTURE: OnceLock<LogCapture> = OnceLock::new();
        CAPTURE.get_or_init(|| {
            let capture = LogCapture::new();
            let subscriber = tracing_subscriber::registry().with(capture.clone());
            tracing::subscriber::set_global_default(subscriber)
                .expect("the log capture is installed once");
            capture
        })
    }

    async fn capture_logs() -> (tokio::sync::MutexGuard<'static, ()>, &'static LogCapture) {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        let guard = LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let capture = capture();
        capture.clear();
        (guard, capture)
    }

    struct FieldVisitor {
        entries: Vec<String>,
    }

    impl Visit for FieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.entries.push(format!("{}={:?}", field.name(), value));
        }
    }

    impl<S: tracing::Subscriber> Layer<S> for LogCapture {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut visitor = FieldVisitor {
                entries: Vec::new(),
            };
            event.record(&mut visitor);
            self.entries.lock().unwrap().push(format!(
                "{} {}",
                event.metadata().level(),
                visitor.entries.join(" ")
            ));
            self.recorded.notify_one();
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn duration_seconds(path: &std::path::Path) -> f64 {
        let output = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-i"])
            .arg(path)
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stderr);
        text.lines()
            .find_map(|line| line.split("Duration: ").nth(1))
            .and_then(|rest| rest.split(',').next())
            .map(|time| {
                let mut parts = time.split(':');
                let hours: f64 = parts.next().unwrap().parse().unwrap();
                let minutes: f64 = parts.next().unwrap().parse().unwrap();
                let seconds: f64 = parts.next().unwrap().parse().unwrap();
                hours * 3600.0 + minutes * 60.0 + seconds
            })
            .expect("ffmpeg reports a duration")
    }

    async fn collect(body: MediaBody) -> Result<Vec<u8>, MediaFetchError> {
        let mut stream = body.chunks;
        let mut collected = Vec::new();
        tokio::time::timeout(HANG_GUARD, async {
            while let Some(chunk) = stream.next().await {
                collected.extend_from_slice(&chunk?);
            }
            Ok(())
        })
        .await
        .expect("the conversion body must finish")?;
        Ok(collected)
    }

    const HANG_GUARD: Duration = Duration::from_secs(10);

    async fn next_chunk(stream: &mut MediaByteStream) -> Bytes {
        tokio::time::timeout(HANG_GUARD, stream.next())
            .await
            .expect("the conversion must emit a chunk")
            .expect("the conversion must not end")
            .expect("the chunk must not be an error")
    }

    async fn on_thread<T: Send + 'static>(
        what: &'static str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(work());
        });
        tokio::time::timeout(HANG_GUARD, receiver)
            .await
            .unwrap_or_else(|_| panic!("{what} did not finish"))
            .unwrap_or_else(|_| panic!("{what} panicked"))
    }

    async fn installed(limit: usize) -> FfmpegConverter {
        let converter = FfmpegConverter::probe(PathBuf::from("ffmpeg"), limit).await;
        assert!(
            converter.available(),
            "ffmpeg must be installed to run the adapter test"
        );
        converter
    }

    fn target(mode: AacMode) -> ConversionTarget {
        ConversionTarget {
            representation: Representation::Aac(mode),
        }
    }

    async fn convert_to_file(source: &std::path::Path, mode: AacMode) -> PathBuf {
        let converter = installed(2).await;
        let body = converter
            .start(ConversionInput::Path(source.to_path_buf()), target(mode))
            .await
            .unwrap();
        let output = std::env::temp_dir().join(format!("whio-l41-{}.aac", uuid::Uuid::now_v7()));
        tokio::fs::write(&output, collect(body).await.unwrap())
            .await
            .unwrap();
        output
    }

    #[tokio::test]
    async fn a_real_ffmpeg_repackages_an_aac_source_without_re_encoding() {
        let source = fixture("aac.m4a");
        let output = convert_to_file(&source, AacMode::Copy).await;

        let expected = duration_seconds(&source);
        let actual = duration_seconds(&output);
        assert!((expected - actual).abs() < 0.1, "{expected} vs {actual}");

        tokio::fs::remove_file(output).await.unwrap();
    }

    #[tokio::test]
    async fn a_real_ffmpeg_encodes_a_lossless_source_to_adts() {
        let source = fixture("lossless.flac");
        let output = convert_to_file(&source, AacMode::Encode { bitrate_kbps: 256 }).await;

        let expected = duration_seconds(&source);
        let actual = duration_seconds(&output);
        assert!((expected - actual).abs() < 0.1, "{expected} vs {actual}");

        tokio::fs::remove_file(output).await.unwrap();
    }

    fn held_input(
        capacity: usize,
    ) -> (mpsc::Sender<Result<Bytes, DeliveryError>>, ConversionInput) {
        let (sender, receiver) = mpsc::channel(capacity);
        let stream: ConversionByteStream = Box::pin(ReceiverStream::new(receiver));
        (sender, ConversionInput::Stream(stream))
    }

    #[tokio::test]
    async fn dropping_the_body_releases_the_source_and_the_conversion_slot() {
        let converter = installed(1).await;
        let (sender, input) = held_input(1);
        let body = converter
            .start(input, target(AacMode::Encode { bitrate_kbps: 256 }))
            .await
            .unwrap();

        drop(body);
        tokio::time::timeout(Duration::from_secs(5), sender.closed())
            .await
            .expect("the abandoned source must be released");

        let permit = converter
            .conversions
            .clone()
            .acquire_many_owned(1)
            .await
            .expect("the conversion slot must be free after teardown");
        drop(permit);
    }

    #[tokio::test]
    async fn a_second_conversion_is_rejected_until_the_first_is_torn_down() {
        let converter = installed(1).await;
        let (sender, input) = held_input(1);
        let body = converter
            .start(input, target(AacMode::Encode { bitrate_kbps: 256 }))
            .await
            .unwrap();

        let (_second_sender, second_input) = held_input(1);
        let error = converter
            .start(second_input, target(AacMode::Encode { bitrate_kbps: 256 }))
            .await
            .err()
            .expect("the second conversion must be rejected");
        assert!(matches!(error, DeliveryError::Overloaded));

        drop(body);
        tokio::time::timeout(Duration::from_secs(5), sender.closed())
            .await
            .expect("the abandoned source must be released");
        let permit = converter
            .conversions
            .clone()
            .acquire_many_owned(1)
            .await
            .expect("the conversion slot must be free after teardown");
        drop(permit);

        let body = converter
            .start(
                ConversionInput::Path(fixture("lossless.flac")),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .expect("a conversion is admitted after teardown");
        drop(body);
    }

    #[cfg(unix)]
    async fn make_fifo(path: &std::path::Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    async fn write_stand_in(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let script = dir.join("stand-in");
        tokio::fs::write(&script, format!("#!/bin/sh\n{body}"))
            .await
            .unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        script
    }

    #[cfg(unix)]
    fn watch_liveness(path: std::path::PathBuf) -> Arc<Notify> {
        let finished = Arc::new(Notify::new());
        let signal = Arc::clone(&finished);
        std::thread::spawn(move || {
            use std::io::Read;

            let Ok(mut file) = std::fs::File::open(&path) else {
                return;
            };
            let mut buffer = [0u8; 64];
            while matches!(file.read(&mut buffer), Ok(count) if count > 0) {}
            signal.notify_one();
        });
        finished
    }

    #[cfg(unix)]
    async fn release_fifo(path: &std::path::Path) {
        let path = path.to_path_buf();
        on_thread("releasing a held stand-in", move || {
            use std::io::Write;

            let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            let _ = file.write_all(b"x");
        })
        .await;
    }

    #[cfg(unix)]
    async fn read_fifo(path: &std::path::Path) {
        let path = path.to_path_buf();
        on_thread("waiting for a stand-in marker", move || {
            use std::io::Read;

            let mut file = std::fs::File::open(path).unwrap();
            let mut byte = [0u8; 1];
            let _ = file.read(&mut byte).unwrap();
        })
        .await;
    }

    #[cfg(unix)]
    async fn wait_for(signal: &Notify, message: &str) {
        tokio::time::timeout(HANG_GUARD, signal.notified())
            .await
            .unwrap_or_else(|_| panic!("{message}"));
    }

    #[cfg(unix)]
    fn converter_for(script: PathBuf) -> FfmpegConverter {
        converter_for_with(script, None)
    }

    #[cfg(unix)]
    fn converter_for_with(script: PathBuf, stdout_eof: Option<Arc<Notify>>) -> FfmpegConverter {
        FfmpegConverter {
            binary: script,
            available: true,
            conversions: Arc::new(Semaphore::new(1)),
            stdout_eof,
        }
    }

    #[cfg(unix)]
    fn held_source() -> (
        mpsc::Sender<Result<Bytes, DeliveryError>>,
        ConversionByteStream,
    ) {
        let (sender, receiver) = mpsc::channel::<Result<Bytes, DeliveryError>>(4);
        (
            sender,
            Box::pin(ReceiverStream::new(receiver)) as ConversionByteStream,
        )
    }

    #[cfg(unix)]
    async fn assert_pending(stream: &mut MediaByteStream, message: &str) {
        let mut next = Box::pin(stream.next());
        std::future::poll_fn(|cx| {
            assert!(next.as_mut().poll(cx).is_pending(), "{message}");
            std::task::Poll::Ready(())
        })
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_feeder_write_failure_fails_a_zero_exit_child() {
        let (_lock, _capture) = capture_logs().await;
        let dir = std::env::temp_dir().join(format!("whio-l41-zero-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let gate = dir.join("gate");
        let live = dir.join("live");
        make_fifo(&gate).await;
        make_fifo(&live).await;
        let script = write_stand_in(
            &dir,
            &format!(
                "exec 3>'{}'\ndd bs=1 count=4 >/dev/null 2>&1\nprintf 'ADTS'\nread _ < '{}'\nexit 0\n",
                live.display(),
                gate.display()
            ),
        )
        .await;

        let liveness = watch_liveness(live);
        let converter = converter_for(script);
        let (sender, stream) = held_source();
        sender.send(Ok(Bytes::from_static(b"0123"))).await.unwrap();
        let body = converter
            .start(
                ConversionInput::Stream(stream),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .unwrap();
        let mut chunks = body.chunks;
        assert_eq!(&next_chunk(&mut chunks).await[..], b"ADTS");
        assert_pending(
            &mut chunks,
            "the body must stay open while the child is held",
        )
        .await;

        release_fifo(&gate).await;
        wait_for(
            &liveness,
            "the zero-exit child must release its liveness pipe",
        )
        .await;
        assert_pending(
            &mut chunks,
            "a zero-exit child must not complete the body while the feeder is held",
        )
        .await;

        sender
            .send(Ok(Bytes::from(vec![0u8; 64 * 1024])))
            .await
            .ok();
        assert!(
            collect(MediaBody {
                content_length: None,
                chunks,
            })
            .await
            .is_err(),
            "a feeder write failure must fail a zero-exit child"
        );
        drop(sender);
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn abandoning_after_stdout_eof_kills_a_held_child() {
        let dir = std::env::temp_dir().join(format!("whio-l41-gate-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let gate = dir.join("gate");
        let closed = dir.join("closed");
        make_fifo(&gate).await;
        make_fifo(&closed).await;
        let script = write_stand_in(
            &dir,
            &format!(
                "dd bs=1 count=4 >/dev/null 2>&1\nprintf 'ADTS'\nexec 1>&-\nprintf 'c' > '{}'\nread _ < '{}'\nexit 0\n",
                closed.display(),
                gate.display()
            ),
        )
        .await;

        let stdout_eof = Arc::new(Notify::new());
        let converter = converter_for_with(script, Some(Arc::clone(&stdout_eof)));
        let (sender, stream) = held_source();
        sender.send(Ok(Bytes::from_static(b"0123"))).await.unwrap();
        let body = converter
            .start(
                ConversionInput::Stream(stream),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .unwrap();
        let mut chunks = body.chunks;
        assert_eq!(&next_chunk(&mut chunks).await[..], b"ADTS");
        read_fifo(&closed).await;
        wait_for(
            &stdout_eof,
            "the supervisor must observe stdout EOF while the child is held",
        )
        .await;
        assert_pending(
            &mut chunks,
            "the body must stay open after stdout closed and before release",
        )
        .await;

        drop(chunks);

        let permit = tokio::time::timeout(
            Duration::from_secs(10),
            converter.conversions.clone().acquire_many_owned(1),
        )
        .await
        .expect("the held child must be killed on abandonment")
        .expect("the conversion slot must be free");
        drop(permit);
        tokio::time::timeout(Duration::from_secs(10), sender.closed())
            .await
            .expect("the abandoned source must be released");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_natural_non_zero_exit_keeps_its_stderr_as_the_primary_detail() {
        let (_lock, capture) = capture_logs().await;

        let dir = std::env::temp_dir().join(format!("whio-l41-nonzero-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let live = dir.join("live");
        make_fifo(&live).await;
        let script = write_stand_in(
            &dir,
            &format!(
                "exec 3>'{}'\ndd bs=1 count=4 >/dev/null 2>&1\necho 'ffmpeg rejected the input' >&2\nexit 7\n",
                live.display()
            ),
        )
        .await;

        let liveness = watch_liveness(live);
        let converter = converter_for(script);
        let (sender, stream) = held_source();
        sender.send(Ok(Bytes::from_static(b"0123"))).await.unwrap();
        let body = converter
            .start(
                ConversionInput::Stream(stream),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .unwrap();

        wait_for(&liveness, "the non-zero child must exit").await;
        sender.send(Err(DeliveryError::Spool)).await.ok();
        assert!(
            collect(body).await.is_err(),
            "the conversion must fail after a non-zero child exit"
        );

        capture.recorded("playback conversion failed").await;
        let entries = capture.matching("playback conversion failed");
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(
            entries[0].contains("ffmpeg rejected the input"),
            "{}",
            entries[0]
        );
        assert!(
            !entries[0].contains("source input failed"),
            "{}",
            entries[0]
        );
        assert!(entries[0].contains("code=7"), "{}", entries[0]);

        drop(sender);
        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn a_non_zero_exit_logs_once_and_redacts_the_path() {
        let (_lock, capture) = capture_logs().await;

        let missing = std::env::temp_dir().join(format!(
            "whio-l41 private secret-{}.m4a",
            uuid::Uuid::now_v7()
        ));
        let converter = installed(1).await;
        let body = converter
            .start(
                ConversionInput::Path(missing.clone()),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .unwrap();
        assert!(collect(body).await.is_err());

        capture.recorded("playback conversion failed").await;
        let entries = capture.matching("playback conversion failed");
        assert_eq!(entries.len(), 1, "{entries:?}");
        let entry = &entries[0];
        assert!(entry.contains("representation="), "{entry}");
        assert!(entry.contains(REDACTED), "{entry}");
        assert!(!entry.contains("private"), "{entry}");
        assert!(!entry.contains("secret-"), "{entry}");
    }

    #[tokio::test]
    async fn a_spawn_failure_logs_once_without_the_binary_path() {
        let (_lock, capture) = capture_logs().await;

        let converter = FfmpegConverter {
            binary: PathBuf::from("/nonexistent/whio-ffmpeg"),
            available: true,
            conversions: Arc::new(Semaphore::new(1)),
            stdout_eof: None,
        };
        let error = converter
            .start(
                ConversionInput::Path(fixture("lossless.flac")),
                target(AacMode::Encode { bitrate_kbps: 256 }),
            )
            .await
            .err()
            .expect("a missing binary must fail");
        assert!(matches!(error, DeliveryError::Conversion));

        capture.recorded("playback conversion failed").await;
        let entries = capture.matching("playback conversion failed");
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(entries[0].contains("representation="), "{}", entries[0]);
        assert!(!entries[0].contains("whio-ffmpeg"), "{}", entries[0]);
    }

    #[tokio::test]
    async fn a_truncated_stderr_tail_is_redacted_through_the_owner_event() {
        let (_lock, capture) = capture_logs().await;

        let name = format!("{}private-secret-.m4a", "private-secret-".repeat(400));
        let long = std::env::temp_dir().join(name);
        let converter = installed(1).await;
        let body = converter
            .start(ConversionInput::Path(long), target(AacMode::Copy))
            .await
            .unwrap();
        assert!(collect(body).await.is_err());

        capture.recorded("playback conversion failed").await;
        let entries = capture.matching("playback conversion failed");
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(entries[0].contains(REDACTED), "{}", entries[0]);
        assert!(!entries[0].contains("private-secret-"), "{}", entries[0]);
    }

    #[test]
    fn a_truncated_stderr_capture_is_redacted() {
        let truncated = StderrCapture {
            text: "private-secret-private-secret-.m4a.".to_owned(),
            truncated: true,
        };
        assert_eq!(sanitize_capture(&truncated), REDACTED);

        let complete = StderrCapture {
            text: "Conversion failed".to_owned(),
            truncated: false,
        };
        assert_eq!(sanitize_capture(&complete), "Conversion failed");
    }

    #[cfg(unix)]
    #[test]
    fn a_natural_non_zero_exit_with_a_feeder_error_keeps_the_ffmpeg_detail() {
        use std::os::unix::process::ExitStatusExt;

        let status = Ok(std::process::ExitStatus::from_raw(7 << 8));
        let feeder = FeederError::Write(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        let stderr = StderrCapture {
            text: "ffmpeg rejected the input".to_owned(),
            truncated: false,
        };

        let outcome = conversion_outcome(Some(&feeder), None, false, &status, &stderr);
        assert!(outcome.failed);
        assert_eq!(outcome.code, Some(7));
        assert!(
            outcome.detail.contains("ffmpeg rejected the input"),
            "{}",
            outcome.detail
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_supervisor_termination_keeps_the_feeder_detail() {
        use std::os::unix::process::ExitStatusExt;

        let killed = Ok(std::process::ExitStatus::from_raw(9));
        let feeder = FeederError::Input;
        let stderr = StderrCapture {
            text: "ffmpeg rejected the input".to_owned(),
            truncated: false,
        };

        let outcome = conversion_outcome(Some(&feeder), None, false, &killed, &stderr);
        assert!(outcome.failed);
        assert_eq!(outcome.detail, "source input failed");
    }

    #[test]
    fn sanitize_removes_paths_and_urls() {
        assert_eq!(
            sanitize("Error opening input file /tmp/l41 private secret-song.m4a."),
            REDACTED
        );
        assert_eq!(
            sanitize("Error opening input file https://host/path?token=secret"),
            REDACTED
        );
        assert_eq!(sanitize("Conversion failed"), "Conversion failed");
        assert_eq!(sanitize(""), "");
    }
}
