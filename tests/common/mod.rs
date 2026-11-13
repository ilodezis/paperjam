//! Shared helpers for integration tests: temp data dir, binary spawning.
#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use paperjam::config::Config;
use paperjam::service::App;

mod openrouter;
#[allow(unused_imports)]
pub use openrouter::{MockOpenRouter, MockReply, RecordedRequest};

pub const SECRET: &str = "integration-test-secret-key-0123456789";

pub struct TestEnv {
    pub dir: tempfile::TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    pub fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// The `paperjam` binary pointed at this environment's data directory.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_paperjam"));
        cmd.current_dir(self.data_dir())
            .env("PAPERJAM_DATA_DIR", self.data_dir())
            .env("PAPERJAM_SECRET_KEY", SECRET)
            // The fixtures are English, Russian and Romanian.
            .env("PAPERJAM_OCR_LANGS", "eng+rus+ron")
            .env_remove("PAPERJAM_NEW_PASSWORD")
            .env_remove("PAPERJAM_LLM_API_KEY")
            .env_remove("PAPERJAM_OPENROUTER_API_KEY");
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run paperjam")
    }

    pub fn run_with_password(&self, args: &[&str], password: &str) -> Output {
        self.command()
            .args(args)
            .env("PAPERJAM_NEW_PASSWORD", password)
            .output()
            .expect("run paperjam")
    }

    pub fn config(&self) -> Config {
        self.config_with(&[])
    }

    /// Like [`TestEnv::config`], with extra environment variables.
    pub fn config_with(&self, vars: &[(&str, &str)]) -> Config {
        let dir = self.data_dir().to_string_lossy().into_owned();
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let mut config = Config::from_lookup(|name| {
            if let Some((_, value)) = vars.iter().find(|(k, _)| k == name) {
                return Some(value.clone());
            }
            match name {
                "PAPERJAM_SECRET_KEY" => Some(SECRET.to_string()),
                "PAPERJAM_DATA_DIR" => Some(dir.clone()),
                // The fixtures are English, Russian and Romanian.
                "PAPERJAM_OCR_LANGS" => Some("eng+rus+ron".to_string()),
                _ => None,
            }
        })
        .expect("test config");
        // The test binary is not `paperjam`: its parsing workers are.
        config.extract_worker_program = Some(PathBuf::from(env!("CARGO_BIN_EXE_paperjam")));
        config.extract_worker_args = vec!["extract-worker".to_string()];
        config
    }

    /// An app whose LLM layer talks to `mock` with a throwaway key.
    pub async fn app_with_llm(&self, mock: &MockOpenRouter, extra: &[(&str, &str)]) -> App {
        let mut vars = vec![
            ("PAPERJAM_LLM_API_KEY", "test-llm-key"),
            ("PAPERJAM_LLM_BASE_URL", mock.base_url.as_str()),
            // The mock listens on loopback; it stands in for OpenRouter unless
            // a test says otherwise through `extra`.
            ("PAPERJAM_LLM_PROVIDER", "openrouter"),
        ];
        vars.retain(|(name, _)| !extra.iter().any(|(e, _)| e == name));
        vars.extend_from_slice(extra);
        App::open(self.config_with(&vars)).await.expect("open app")
    }

    /// Starts `paperjam serve` on a free loopback port and waits until it
    /// reports where it listens.
    pub fn serve(&self) -> Server {
        self.serve_with(&[])
    }

    /// Like [`TestEnv::serve`], with extra environment variables.
    pub fn serve_with(&self, vars: &[(&str, &str)]) -> Server {
        let mut command = self.command();
        command.env("PAPERJAM_BIND", "127.0.0.1:0");
        command.envs(vars.iter().copied());
        let mut child = command
            .arg("serve")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start paperjam serve");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(url) = line.strip_prefix("listening on ") {
                    let _ = sender.send(url.trim().replace("0.0.0.0", "127.0.0.1"));
                }
            }
        });
        let base = receiver
            .recv_timeout(Duration::from_secs(60))
            .expect("the server did not report its address");
        Server { base, child }
    }

    /// An app that reads scans with only the given Tesseract languages.
    pub async fn app_with_langs(&self, langs: &str) -> App {
        let config = self.config_with(&[("PAPERJAM_OCR_LANGS", langs)]);
        App::open(config).await.expect("open app")
    }

    pub async fn app(&self) -> App {
        App::open(self.config()).await.expect("open app")
    }
}

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// True when `tesseract` and every language pack in `langs` is installed;
/// otherwise prints a one-line skip message and returns false.
pub fn have_tesseract(langs: &[&str]) -> bool {
    let Ok(out) = Command::new("tesseract").arg("--list-langs").output() else {
        eprintln!("skipped: tesseract is not installed");
        return false;
    };
    let listed = String::from_utf8_lossy(&out.stdout).into_owned();
    for lang in langs {
        if !listed.lines().any(|l| l.trim() == *lang) {
            eprintln!("skipped: tesseract language pack `{lang}` is missing");
            return false;
        }
    }
    true
}

/// A file of the synthetic corpus (`cargo xtask fixtures` writes it).
pub fn fixture(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("gen")
        .join(name);
    assert!(
        path.exists(),
        "fixture {name} is missing: run `cargo xtask fixtures` first"
    );
    path
}

/// A running `paperjam serve` process; killed on drop.
pub struct Server {
    pub base: String,
    child: Child,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
