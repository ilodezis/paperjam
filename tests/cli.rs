use std::process::Command;

#[test]
fn version_flag_prints_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_paperjam"))
        .arg("--version")
        .output()
        .expect("run paperjam --version");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        format!("paperjam {}\n", env!("CARGO_PKG_VERSION"))
    );
}

fn reindex_stderr(log_format: Option<&str>, rust_log: Option<&str>) -> String {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_paperjam"));
    cmd.arg("reindex")
        .current_dir(dir.path())
        .env("PAPERJAM_SECRET_KEY", "k".repeat(40))
        .env("PAPERJAM_DATA_DIR", dir.path().join("data"))
        .env_remove("PAPERJAM_LOG_FORMAT")
        .env_remove("RUST_LOG");
    if let Some(v) = log_format {
        cmd.env("PAPERJAM_LOG_FORMAT", v);
    }
    if let Some(v) = rust_log {
        cmd.env("RUST_LOG", v);
    }
    let out = cmd.output().expect("run paperjam reindex");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stderr).unwrap()
}

#[test]
fn logs_go_to_stderr_as_text_at_info_by_default() {
    let err = reindex_stderr(None, None);
    assert!(err.contains("search index rebuilt"), "{err}");
    assert!(!err.trim_start().starts_with('{'), "{err}");
}

#[test]
fn log_format_json_writes_one_object_per_line() {
    let err = reindex_stderr(Some("json"), None);
    let line = err
        .lines()
        .find(|l| l.contains("search index rebuilt"))
        .unwrap_or_else(|| panic!("no rebuild event in {err}"));
    let event: serde_json::Value = serde_json::from_str(line).expect("a JSON line");
    assert_eq!(event["level"], "INFO");
}

#[test]
fn rust_log_filters_events_out() {
    let err = reindex_stderr(None, Some("error"));
    assert!(!err.contains("search index rebuilt"), "{err}");
}

#[test]
fn unknown_log_format_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_paperjam"))
        .arg("reindex")
        .current_dir(dir.path())
        .env("PAPERJAM_LOG_FORMAT", "yaml")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PAPERJAM_LOG_FORMAT"));
}
