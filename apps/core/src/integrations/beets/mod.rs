pub mod config;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::watch,
};
use uuid::Uuid;

use crate::features::acquisition::ports::{
    AcquisitionTarget, ImportError, ImportJob, ImportOutcome, ImportedItem, Importer,
};
use crate::platform::timer::{Timer, TokioTimer};

const LIST_FORMAT: &str = "$mb_albumid $mb_trackid $path";
const READ_CHUNK: usize = 8192;

pub struct Beets {
    command: String,
    config: PathBuf,
    library_dir: PathBuf,
    deadline: Duration,
    stall: Duration,
    work_root: PathBuf,
    timer: Arc<dyn Timer>,
    dispatch: Option<tracing::Dispatch>,
    #[cfg(test)]
    activity_observer: Option<watch::Sender<Duration>>,
}

impl Beets {
    pub fn new(config: &config::Config, library_dir: PathBuf) -> Self {
        Self {
            command: config.command.clone(),
            config: config.config.clone(),
            library_dir,
            deadline: config.deadline,
            stall: config.stall,
            work_root: std::env::temp_dir(),
            timer: Arc::new(TokioTimer::new()),
            dispatch: None,
            #[cfg(test)]
            activity_observer: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_work_root(
        config: &config::Config,
        library_dir: PathBuf,
        work_root: PathBuf,
        timer: Arc<dyn Timer>,
    ) -> Self {
        Self {
            command: config.command.clone(),
            config: config.config.clone(),
            library_dir,
            deadline: config.deadline,
            stall: config.stall,
            work_root,
            timer,
            dispatch: None,
            activity_observer: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_dispatch(mut self, dispatch: tracing::Dispatch) -> Self {
        self.dispatch = Some(dispatch);
        self
    }

    #[cfg(test)]
    fn with_activity(mut self, activity: watch::Sender<Duration>) -> Self {
        self.activity_observer = Some(activity);
        self
    }

    async fn supervise(
        &self,
        child: &mut Child,
        group: &ProcessGroup,
        activity: &mut watch::Receiver<Duration>,
        deadline: Duration,
    ) -> Result<ExitStatus, ImportError> {
        loop {
            let stall_at = *activity.borrow() + self.stall;

            tokio::select! {
                biased;
                _ = self.timer.sleep_until(deadline) => {
                    group.kill();
                    let _ = child.wait().await;
                    return Err(ImportError::TimedOut);
                }
                _ = self.timer.sleep_until(stall_at) => {
                    if !stall_elapsed(self.timer.now(), *activity.borrow(), self.stall) {
                        continue;
                    }

                    group.kill();
                    let _ = child.wait().await;
                    return Err(ImportError::Stalled);
                }
                status = child.wait() => return status.map_err(wait_error),
                _ = activity.changed() => continue,
            }
        }
    }

    async fn run_import(
        &self,
        job: &ImportJob,
        library: &Path,
        deadline: Duration,
    ) -> Result<ImportOutcome, ImportError> {
        let mut args = self.common_args(library);
        args.extend([OsString::from("import"), OsString::from("-q")]);

        if matches!(job.target, AcquisitionTarget::Recording(_)) {
            args.push(OsString::from("-s"));
        }

        args.extend([
            OsString::from("--search-id"),
            OsString::from(job.target.id().to_string()),
            job.path.clone().into_os_string(),
        ]);

        let (status, _) = self.run(&args, false, deadline).await?;
        if !status.success() {
            return Err(ImportError::Failed {
                status: status.code(),
            });
        }

        let mut listing = self.common_args(library);
        listing.extend([
            OsString::from("ls"),
            OsString::from("-f"),
            OsString::from(LIST_FORMAT),
        ]);
        let (status, output) = self.run(&listing, true, deadline).await?;
        if !status.success() {
            return Err(ImportError::Failed {
                status: status.code(),
            });
        }

        Ok(ImportOutcome {
            items: parse_items(output.as_deref().unwrap_or_default()),
        })
    }

    fn common_args(&self, library: &Path) -> Vec<OsString> {
        vec![
            OsString::from("-c"),
            self.config.clone().into_os_string(),
            OsString::from("-l"),
            library.to_path_buf().into_os_string(),
            OsString::from("-d"),
            self.library_dir.clone().into_os_string(),
            OsString::from("-v"),
        ]
    }

    async fn run(
        &self,
        args: &[OsString],
        capture: bool,
        deadline: Duration,
    ) -> Result<(ExitStatus, Option<String>), ImportError> {
        let mut command = Command::new(&self.command);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(spawn_error)?;
        let group = ProcessGroup::of(&child);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (activity_tx, activity) = watch::channel(self.timer.now());
        #[cfg(test)]
        let (activity_tx, activity) = match &self.activity_observer {
            Some(observer) => (observer.clone(), observer.subscribe()),
            None => (activity_tx, activity),
        };
        let mut activity = activity;
        let collected = capture.then(|| Arc::new(parking_lot::Mutex::new(Vec::<u8>::new())));
        let mut readers = Readers::default();

        if let Some(stream) = stdout {
            readers.push(read_stream(
                stream,
                collected.clone(),
                activity_tx.clone(),
                Arc::clone(&self.timer),
                self.dispatch.clone(),
            ));
        }
        if let Some(stream) = stderr {
            readers.push(read_stream(
                stream,
                None,
                activity_tx.clone(),
                Arc::clone(&self.timer),
                self.dispatch.clone(),
            ));
        }

        let outcome = self
            .supervise(&mut child, &group, &mut activity, deadline)
            .await;

        group.kill();
        let status = finish_run(outcome, &mut readers, &self.timer, deadline).await?;

        let output =
            collected.map(|collected| String::from_utf8_lossy(&collected.lock()).into_owned());

        Ok((status, output))
    }
}

#[async_trait]
impl Importer for Beets {
    #[tracing::instrument(skip_all, fields(backend = "beets", kind = job.target.kind()))]
    async fn import(&self, job: &ImportJob) -> Result<ImportOutcome, ImportError> {
        let scratch = Scratch::new(
            self.work_root
                .join(format!("whio-beets-{}", Uuid::now_v7())),
        )
        .create(|path| std::fs::create_dir_all(path))
        .await?;
        let library = scratch.path.join("library.db");
        let deadline = self.timer.now() + self.deadline;

        self.run_import(job, &library, deadline).await
    }
}

#[derive(Default)]
struct Readers {
    handles: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Readers {
    fn push(
        &mut self,
        reader: impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
    ) {
        self.handles.push(tokio::spawn(reader));
    }

    fn abort(&self) {
        for handle in &self.handles {
            handle.abort();
        }
    }

    async fn next(&mut self) -> Result<std::io::Result<()>, tokio::task::JoinError> {
        let (index, result) = std::future::poll_fn(|cx| {
            use std::{future::Future, pin::Pin, task::Poll};

            for (index, handle) in self.handles.iter_mut().enumerate() {
                if let Poll::Ready(result) = Pin::new(handle).poll(cx) {
                    return Poll::Ready((index, result));
                }
            }

            Poll::Pending
        })
        .await;
        self.handles.remove(index);

        result
    }

    async fn join(&mut self) -> Result<(), ImportError> {
        while !self.handles.is_empty() {
            match self.next().await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::debug!(%error, "beets output read failed");
                    return Err(ImportError::Failed { status: None });
                }
                Err(_) => return Err(ImportError::Failed { status: None }),
            }
        }

        Ok(())
    }

    async fn drain(
        &mut self,
        timer: &Arc<dyn Timer>,
        deadline: Duration,
    ) -> Result<(), ImportError> {
        let result = tokio::select! {
            biased;
            result = self.join() => result,
            _ = timer.sleep_until(deadline) => Err(ImportError::TimedOut),
        };
        if result.is_err() {
            self.abort();
            while !self.handles.is_empty() {
                let _ = self.next().await;
            }
        }

        if result.is_ok() && timer.now() >= deadline {
            Err(ImportError::TimedOut)
        } else {
            result
        }
    }
}

impl Drop for Readers {
    fn drop(&mut self) {
        self.abort();
    }
}

async fn finish_run(
    outcome: Result<ExitStatus, ImportError>,
    readers: &mut Readers,
    timer: &Arc<dyn Timer>,
    deadline: Duration,
) -> Result<ExitStatus, ImportError> {
    let drained = readers.drain(timer, deadline).await;
    match (outcome, drained) {
        (Err(error @ ImportError::Failed { .. }), _) => Err(error),
        (_, Err(error)) => Err(error),
        (outcome, Ok(())) => outcome,
    }
}

struct Scratch {
    path: PathBuf,
    #[cfg(test)]
    cleanup_ack: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Scratch {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(test)]
            cleanup_ack: None,
        }
    }

    async fn create(
        self,
        create: impl FnOnce(&Path) -> std::io::Result<()> + Send + 'static,
    ) -> Result<Self, ImportError> {
        tokio::task::spawn_blocking(move || {
            create(&self.path).map_err(wait_error)?;

            Ok(self)
        })
        .await
        .map_err(|_| ImportError::Failed { status: None })?
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        #[cfg(test)]
        if let Some(ack) = self.cleanup_ack.take() {
            let _ = ack.send(());
        }
    }
}

async fn read_stream<R: AsyncRead + Unpin>(
    mut stream: R,
    collected: Option<Arc<parking_lot::Mutex<Vec<u8>>>>,
    activity: watch::Sender<Duration>,
    timer: Arc<dyn Timer>,
    dispatch: Option<tracing::Dispatch>,
) -> std::io::Result<()> {
    let mut buffer = [0u8; READ_CHUNK];
    let mut bytes = 0usize;

    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                bytes += read;

                if let Some(collected) = &collected {
                    collected.lock().extend_from_slice(&buffer[..read]);
                }

                let _ = activity.send_replace(timer.now());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }

    match &dispatch {
        Some(dispatch) => tracing::dispatcher::with_default(dispatch, || {
            tracing::debug!(bytes, "beets output read");
        }),
        None => tracing::debug!(bytes, "beets output read"),
    }

    Ok(())
}

#[cfg(unix)]
struct ProcessGroup {
    pid: i32,
}

#[cfg(unix)]
impl ProcessGroup {
    fn of(child: &Child) -> Self {
        Self {
            pid: child.id().map(|pid| pid as i32).unwrap_or_default(),
        }
    }

    fn kill(&self) {
        if self.pid != 0 {
            unsafe {
                libc::killpg(self.pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(not(unix))]
struct ProcessGroup;

#[cfg(not(unix))]
impl ProcessGroup {
    fn of(_child: &Child) -> Self {
        Self
    }

    fn kill(&self) {}
}

#[cfg(not(unix))]
impl Drop for ProcessGroup {
    fn drop(&mut self) {}
}

fn stall_elapsed(now: Duration, last_activity: Duration, stall: Duration) -> bool {
    last_activity + stall <= now
}

fn spawn_error(error: std::io::Error) -> ImportError {
    if error.kind() == std::io::ErrorKind::NotFound {
        ImportError::Unavailable
    } else {
        ImportError::Failed { status: None }
    }
}

fn wait_error(_error: std::io::Error) -> ImportError {
    ImportError::Failed { status: None }
}

fn parse_items(output: &str) -> Vec<ImportedItem> {
    output.lines().filter_map(parse_item).collect()
}

fn parse_item(line: &str) -> Option<ImportedItem> {
    let mut fields = line.splitn(3, ' ');
    let release = fields.next().unwrap_or_default();
    let recording = fields.next().unwrap_or_default();
    let path = fields.next().unwrap_or_default();

    if path.is_empty() {
        return None;
    }

    Some(ImportedItem {
        release_id: nonempty(release),
        recording_id: nonempty(recording),
        path: PathBuf::from(path),
    })
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}
