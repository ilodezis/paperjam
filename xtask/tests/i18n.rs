//! `cargo xtask i18n-check`: catalogs must not drift apart.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(dir: &Path, name: &str, json: &str) {
    fs::write(dir.join(name), json).unwrap();
}

fn check(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .arg("i18n-check")
        .arg("--dir")
        .arg(dir)
        .output()
        .expect("run xtask")
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const EN: &str = r#"{
  "a": {"title": "Title", "count_one": "{{count}} item", "count_other": "{{count}} items"},
  "b": "Bee"
}"#;

const RU_OK: &str = r#"{
  "a": {"title": "Заголовок", "count_one": "{{count}} штука", "count_few": "{{count}} штуки",
        "count_many": "{{count}} штук", "count_other": "{{count}} штуки"},
  "b": "Пчела"
}"#;

#[test]
fn matching_catalogs_pass() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "en.json", EN);
    write(dir.path(), "ru.json", RU_OK);
    let out = check(dir.path());
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn extra_key_in_ru_fails() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "en.json", EN);
    write(
        dir.path(),
        "ru.json",
        &RU_OK.replace("\"b\": \"Пчела\"", "\"b\": \"Пчела\", \"c\": \"Лишний\""),
    );
    let out = check(dir.path());
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("c"), "{}", text(&out));
}

#[test]
fn missing_translation_only_warns() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "en.json", EN);
    write(
        dir.path(),
        "ru.json",
        r#"{"a": {"title": "Заголовок", "count_one": "1", "count_few": "2", "count_many": "5", "count_other": "x"}}"#,
    );
    let out = check(dir.path());
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).to_lowercase().contains("warning"),
        "{}",
        text(&out)
    );
}

#[test]
fn russian_plural_needs_few_and_many() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "en.json", EN);
    write(
        dir.path(),
        "ru.json",
        r#"{"a": {"title": "Заголовок", "count_one": "1", "count_other": "x"}, "b": "Пчела"}"#,
    );
    let out = check(dir.path());
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("count"), "{}", text(&out));
}

#[test]
fn the_repository_catalogs_are_consistent() {
    let catalogs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("web/src/locales");
    let out = check(&catalogs);
    assert!(out.status.success(), "{}", text(&out));
}
