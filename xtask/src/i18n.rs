//! Locale catalog consistency (`cargo xtask i18n-check`).
//!
//! `en.json` is the source. Another catalog may not contain keys English
//! lacks (error) and its plural keys must have every form its language needs
//! (error); keys it is missing only warn, because the UI falls back to
//! English.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, bail};
use serde_json::Value;

#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

const PLURAL_SUFFIXES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// Flattens nested objects into dotted keys.
fn flatten(prefix: &str, value: &Value, out: &mut BTreeMap<String, String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let full = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(&full, child, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

fn load(path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let value: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
    let mut out = BTreeMap::new();
    flatten("", &value, &mut out);
    Ok(out)
}

/// `a.count_few` → (`a.count`, Some("few")); other keys → (key, None).
fn split_plural(key: &str) -> (String, Option<&'static str>) {
    for suffix in PLURAL_SUFFIXES {
        if let Some(base) = key.strip_suffix(&format!("_{suffix}")) {
            return (base.to_string(), Some(suffix));
        }
    }
    (key.to_string(), None)
}

/// The plural forms a language needs (CLDR, for the languages we ship).
fn required_forms(language: &str) -> &'static [&'static str] {
    match language {
        "ru" | "uk" | "pl" => &["one", "few", "many", "other"],
        _ => &["one", "other"],
    }
}

fn bases(catalog: &BTreeMap<String, String>) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut out: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for key in catalog.keys() {
        let (base, form) = split_plural(key);
        let forms = out.entry(base).or_default();
        if let Some(form) = form {
            forms.insert(form);
        }
    }
    out
}

pub fn check(dir: &Path) -> anyhow::Result<Report> {
    let english = load(&dir.join("en.json"))?;
    let english_bases = bases(&english);
    let mut report = Report::default();

    let mut others = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if name != "en.json" && name.ends_with(".json") {
            others.push(path);
        }
    }
    others.sort();
    if others.is_empty() {
        bail!("no catalog besides en.json in {}", dir.display());
    }

    for path in others {
        let language = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let catalog = load(&path)?;
        let own = bases(&catalog);

        for (base, forms) in &own {
            let Some(english_forms) = english_bases.get(base) else {
                report.errors.push(format!(
                    "{language}: key `{base}` does not exist in en.json"
                ));
                continue;
            };
            // A plural group must be complete for the language.
            if !english_forms.is_empty() || !forms.is_empty() {
                for form in required_forms(&language) {
                    if !forms.contains(form) {
                        report.errors.push(format!(
                            "{language}: plural `{base}` lacks the `_{form}` form"
                        ));
                    }
                }
            }
        }
        for base in english_bases.keys() {
            if !own.contains_key(base) {
                report.warnings.push(format!(
                    "{language}: `{base}` is not translated (English is used)"
                ));
            }
        }
    }
    Ok(report)
}
