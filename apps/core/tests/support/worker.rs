#![allow(dead_code)]

use std::{
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    str::FromStr,
    time::Duration,
};

use sqlx::{ConnectOptions, PgPool, postgres::PgConnectOptions};

pub const CREDENTIAL_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

pub fn database_url(pool: &PgPool) -> String {
    let database = pool
        .connect_options()
        .get_database()
        .expect("a test pool names its database")
        .to_owned();
    PgConnectOptions::from_str(&std::env::var("DATABASE_URL").expect("DATABASE_URL"))
        .expect("DATABASE_URL is a postgres URL")
        .database(&database)
        .to_url_lossy()
        .to_string()
}

fn free_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port");
    listener.local_addr().expect("a local address")
}

pub struct Worker {
    child: Child,
    address: SocketAddr,
    log: PathBuf,
}

impl Worker {
    pub fn start(pool: &PgPool, queues: &str) -> Self {
        Self::start_with(pool, queues, &[])
    }

    pub fn spawn(queues: &str, variables: &[(&str, &str)]) -> Self {
        let address = free_address();
        let log = std::env::temp_dir().join(format!(
            "whio-worker-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("a clock")
                .as_nanos()
        ));
        let output = std::fs::File::create(&log).expect("a worker log file");

        let mut command = Command::new(env!("CARGO_BIN_EXE_whio-worker"));
        command
            .env("WHIO_DATABASE_URL", "postgres://whio:whio@127.0.0.1:1/whio")
            .env("WHIO_CREDENTIAL_KEY", CREDENTIAL_KEY)
            .env("WHIO_WORKER_QUEUES", queues)
            .env("WHIO_WORKER_BIND_ADDRESS", address.to_string())
            .env("WHIO_LOG_LEVEL", "info")
            .stdout(Stdio::from(output.try_clone().expect("a log handle")))
            .stderr(Stdio::from(output));

        for (name, value) in variables {
            command.env(name, value);
        }

        let child = command.spawn().expect("the whio-worker binary");

        Self {
            child,
            address,
            log,
        }
    }

    pub fn start_with(pool: &PgPool, queues: &str, variables: &[(&str, &str)]) -> Self {
        let database_url = database_url(pool);
        let mut all = vec![("WHIO_DATABASE_URL", database_url.as_str())];
        all.extend_from_slice(variables);

        Self::spawn(queues, &all)
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    pub async fn await_health(&mut self, path: &str, expected: u16) -> serde_json::Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);

        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "worker exited with {status} before {path} answered {expected}:\n{}",
                    self.log()
                );
            }

            if let Ok(response) = reqwest::get(format!("http://{}{path}", self.address)).await
                && response.status().as_u16() == expected
            {
                return response.json().await.expect("a JSON health body");
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "worker did not answer {path} with {expected}:\n{}",
                self.log()
            );

            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn await_draining(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);

        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "worker exited with {status} before it was observed draining:\n{}",
                    self.log()
                );
            }

            let ready = reqwest::get(format!("http://{}/health/ready", self.address)).await;
            let live = reqwest::get(format!("http://{}/health/live", self.address)).await;

            if let (Ok(ready), Ok(live)) = (ready, live)
                && (ready.status().as_u16(), live.status().as_u16()) == (503, 200)
            {
                let body: serde_json::Value = ready.json().await.expect("a JSON health body");
                assert_eq!(body["shutting_down"], true);
                return;
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "worker was never observed draining:\n{}",
                self.log()
            );

            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    pub async fn status(&self, path: &str) -> u16 {
        reqwest::get(format!("http://{}{path}", self.address))
            .await
            .expect("the worker to answer")
            .status()
            .as_u16()
    }

    pub fn interrupt(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill to run");
        assert!(status.success(), "SIGTERM was delivered");
    }

    pub async fn await_exit(&mut self, within: Duration) -> ExitStatus {
        let deadline = tokio::time::Instant::now() + within;

        loop {
            if let Some(status) = self.child.try_wait().expect("the worker status") {
                return status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "worker did not exit within {within:?}:\n{}",
                self.log()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.log);
    }
}
