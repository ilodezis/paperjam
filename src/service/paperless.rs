//! Import of a Paperless-ngx `document_exporter` directory: the original
//! files go through the normal upload path, and the metadata a person curated
//! (title, date, correspondent, tags, document type) is stored as manual
//! values so the LLM pass never overwrites it.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use chrono::NaiveDate;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::hashing::to_hex;
use crate::ids::{CorrespondentId, TagId, UserId};
use crate::model::document::{DocumentPatch, OrganizationLink, Role};
use crate::model::taxonomy::NewCorrespondent;
use crate::service::upload::{UploadInput, UploadOutcome, upload};
use crate::service::{Actor, App, Via, documents, taxonomy};

const FILE_KEY: &str = "__exported_file_name__";
const MAX_REASONS: usize = 1000;

/// One document of the export, reduced to what is imported.
#[derive(Debug, Clone)]
struct Item {
    pk: i64,
    title: Option<String>,
    date: Option<NaiveDate>,
    correspondent: Option<String>,
    tags: Vec<String>,
    document_type: Option<String>,
    file: Option<PathBuf>,
    original_name: Option<String>,
    mime: Option<String>,
    trashed: bool,
}

#[derive(Debug, Default)]
struct Export {
    items: Vec<Item>,
    notes: usize,
}

/// The outcome of a run. Failures carry the Paperless document id and a
/// fixed reason, never titles or file names.
#[derive(Debug, Default)]
pub struct Report {
    pub dry_run: bool,
    pub imported: usize,
    pub duplicates: usize,
    pub restored: usize,
    pub trashed_in_paperless: usize,
    pub failed: Vec<(i64, String)>,
    pub tags_created: usize,
    pub tags_skipped: usize,
    pub correspondents_created: usize,
    pub notes_dropped: usize,
}

/// `--user`, or the one enabled administrator.
pub async fn resolve_user(app: &App, username: Option<&str>) -> anyhow::Result<UserId> {
    let ctx = app.ctx(Actor::cli());
    if let Some(name) = username {
        let user = crate::service::users::find_user_by_username(&ctx, name)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if user.disabled_at.is_some() {
            bail!("that user is disabled");
        }
        return Ok(user.id);
    }
    let admins: Vec<_> = crate::service::users::list_users(&ctx)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .filter(|u| u.is_admin && u.disabled_at.is_none())
        .collect();
    match admins.as_slice() {
        [only] => Ok(only.id),
        [] => bail!("there is no administrator; pass --user <name>"),
        _ => bail!("there are several administrators; pass --user <name>"),
    }
}

/// `manifest.json` first, then the per-document `*-manifest.json` files a
/// `--split-manifest` export adds.
fn manifest_files(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut sidecars = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).context("reading the export directory")? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("-manifest.json"))
            {
                sidecars.push(path);
            }
        }
    }
    sidecars.sort();
    let mut files = vec![dir.join("manifest.json")];
    files.extend(sidecars);
    Ok(files)
}

fn read_manifest(path: &Path) -> anyhow::Result<Vec<Value>> {
    let bytes = std::fs::read(path).context("reading a manifest file")?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Array(records)) => Ok(records),
        Ok(_) => bail!("a manifest file is not a JSON array: is this a Paperless-ngx export?"),
        Err(e) => bail!("a manifest file is not valid JSON (line {})", e.line()),
    }
}

fn parse_date(raw: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(raw.get(..10)?, "%Y-%m-%d").ok()
}

fn text(fields: &Value, key: &str) -> Option<String> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn load_export(dir: &Path) -> anyhow::Result<Export> {
    if dir.is_file() {
        bail!("expected the export directory; unzip a --zip export first");
    }
    if !dir.join("manifest.json").is_file() {
        bail!("no manifest.json in that directory: is it a Paperless-ngx export?");
    }
    if let Ok(meta) = std::fs::read(dir.join("metadata.json"))
        && let Ok(meta) = serde_json::from_slice::<Value>(&meta)
        && meta.get("__crypto__").is_some()
    {
        bail!("the export is encrypted; export again without a passphrase");
    }

    let mut names: HashMap<(&'static str, i64), String> = HashMap::new();
    let mut docs: Vec<Value> = Vec::new();
    let mut seen_docs = HashSet::new();
    let mut notes = BTreeSet::new();
    for path in manifest_files(dir)? {
        for record in read_manifest(&path)? {
            let Some(pk) = record.get("pk").and_then(Value::as_i64) else {
                continue;
            };
            let kind = match record.get("model").and_then(Value::as_str) {
                Some("documents.tag") => "tag",
                Some("documents.correspondent") => "correspondent",
                Some("documents.documenttype") => "type",
                Some("documents.note") => {
                    notes.insert(pk);
                    continue;
                }
                Some("documents.document") => {
                    if seen_docs.insert(pk) {
                        docs.push(record);
                    }
                    continue;
                }
                _ => continue,
            };
            if let Some(name) = record.get("fields").and_then(|f| text(f, "name")) {
                names.insert((kind, pk), name);
            }
        }
    }

    let lookup = |kind: &'static str, v: Option<&Value>| {
        v.and_then(Value::as_i64)
            .and_then(|pk| names.get(&(kind, pk)).cloned())
    };
    let items = docs
        .iter()
        .map(|record| {
            let fields = record.get("fields").unwrap_or(&Value::Null);
            Item {
                pk: record["pk"].as_i64().unwrap_or_default(),
                title: text(fields, "title"),
                date: fields
                    .get("created")
                    .and_then(Value::as_str)
                    .and_then(parse_date),
                correspondent: lookup("correspondent", fields.get("correspondent")),
                tags: fields
                    .get("tags")
                    .and_then(Value::as_array)
                    .map(|t| t.iter().filter_map(|v| lookup("tag", Some(v))).collect())
                    .unwrap_or_default(),
                document_type: lookup("type", fields.get("document_type")),
                file: record
                    .get(FILE_KEY)
                    .and_then(Value::as_str)
                    .map(PathBuf::from),
                original_name: text(fields, "original_filename"),
                mime: text(fields, "mime_type"),
                trashed: fields.get("deleted_at").is_some_and(|v| !v.is_null()),
            }
        })
        .collect();
    Ok(Export {
        items,
        notes: notes.len(),
    })
}

/// The export-relative path, refused when it could leave the directory.
/// `dir` must be canonical; symlinks are followed only while they stay inside it.
fn resolve_file(dir: &Path, rel: &Path) -> Result<PathBuf, &'static str> {
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("the file path points outside the export");
    }
    let Ok(path) = dir.join(rel).canonicalize() else {
        return Err("the exported file is missing");
    };
    if !path.starts_with(dir) {
        return Err("the file path points outside the export");
    }
    if path.is_file() {
        Ok(path)
    } else {
        Err("the exported file is missing")
    }
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(to_hex(&hasher.finalize()));
        }
        hasher.update(&buf[..n]);
    }
}

fn tag_display(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Comparison key; the tags table is unique case-insensitively.
fn tag_key(name: &str) -> String {
    tag_display(name).to_lowercase()
}

/// Paperless tags, plus the document type as `paperless:<type>`: paperjam's
/// types are a fixed typed set, so the Paperless ones have no counterpart.
fn wanted_tags(item: &Item) -> Vec<String> {
    let mut tags = item.tags.clone();
    if let Some(t) = &item.document_type {
        tags.push(format!("paperless:{t}"));
    }
    tags.retain(|t| !tag_key(t).is_empty());
    tags
}

fn correspondent_of(item: &Item) -> Option<String> {
    item.correspondent
        .as_deref()
        .and_then(crate::llm::normalize::correspondent_name)
}

/// Existing tags and correspondents by comparison key, plus the keys a dry
/// run found missing.
struct Vocabulary {
    tags: HashMap<String, TagId>,
    correspondents: HashMap<String, CorrespondentId>,
    planned_tags: HashSet<String>,
    planned_correspondents: HashSet<String>,
}

impl Vocabulary {
    async fn load(app: &App) -> anyhow::Result<Self> {
        let tags: Vec<(TagId, String)> = sqlx::query_as("SELECT id, name FROM tags")
            .fetch_all(&app.db.read)
            .await?;
        let orgs: Vec<(CorrespondentId, String, String)> =
            sqlx::query_as("SELECT id, name, aliases FROM correspondents")
                .fetch_all(&app.db.read)
                .await?;
        let mut correspondents = HashMap::new();
        for (id, name, aliases) in orgs {
            correspondents.insert(crate::matching::name_key(&name), id);
            for alias in serde_json::from_str::<Vec<String>>(&aliases).unwrap_or_default() {
                correspondents
                    .entry(crate::matching::name_key(&alias))
                    .or_insert(id);
            }
        }
        Ok(Self {
            tags: tags.into_iter().map(|(id, n)| (tag_key(&n), id)).collect(),
            correspondents,
            planned_tags: HashSet::new(),
            planned_correspondents: HashSet::new(),
        })
    }

    /// Tags and correspondent for an item, creating what is missing.
    async fn resolve(
        &mut self,
        app: &App,
        actor: Actor,
        item: &Item,
        report: &mut Report,
    ) -> (Vec<TagId>, Option<CorrespondentId>, bool) {
        let ctx = app.ctx(actor);
        let mut incomplete = false;
        let mut tags = Vec::new();
        for name in wanted_tags(item) {
            let key = tag_key(&name);
            let id = match self.tags.get(&key) {
                Some(id) => *id,
                None => match taxonomy::create_tag(&ctx, &tag_display(&name), None).await {
                    Ok(tag) => {
                        self.tags.insert(key, tag.id);
                        report.tags_created += 1;
                        tag.id
                    }
                    Err(_) => {
                        report.tags_skipped += 1;
                        incomplete = true;
                        continue;
                    }
                },
            };
            if !tags.contains(&id) {
                tags.push(id);
            }
        }
        let mut org = None;
        if let Some(name) = correspondent_of(item) {
            let key = crate::matching::name_key(&name);
            org = match self.correspondents.get(&key) {
                Some(id) => Some(*id),
                None => match taxonomy::create_correspondent(
                    &ctx,
                    NewCorrespondent {
                        name,
                        aliases: Vec::new(),
                    },
                )
                .await
                {
                    Ok(c) => {
                        self.correspondents.insert(key, c.id);
                        report.correspondents_created += 1;
                        Some(c.id)
                    }
                    Err(_) => {
                        incomplete = true;
                        None
                    }
                },
            };
        }
        (tags, org, incomplete)
    }

    /// Dry run: counts what `resolve` would create.
    fn plan(&mut self, item: &Item, report: &mut Report) {
        for name in wanted_tags(item) {
            let key = tag_key(&name);
            if self.tags.contains_key(&key) {
                continue;
            }
            if tag_display(&name).chars().count() > taxonomy::MAX_TAG_CHARS {
                report.tags_skipped += 1;
            } else if self.planned_tags.insert(key) {
                report.tags_created += 1;
            }
        }
        if let Some(name) = correspondent_of(item) {
            let key = crate::matching::name_key(&name);
            if !self.correspondents.contains_key(&key) && self.planned_correspondents.insert(key) {
                report.correspondents_created += 1;
            }
        }
    }
}

pub async fn import(app: &App, dir: &Path, user: UserId, dry_run: bool) -> anyhow::Result<Report> {
    let export = load_export(dir)?;
    let dir = &dir.canonicalize().context("reading the export directory")?;
    let actor = Actor::user(user, Via::Cli);
    let ctx = app.ctx(actor);
    let mut vocab = Vocabulary::load(app).await?;
    let mut planned = HashSet::new();
    let mut report = Report {
        dry_run,
        notes_dropped: export.notes,
        ..Report::default()
    };

    for item in &export.items {
        if item.trashed {
            report.trashed_in_paperless += 1;
            continue;
        }
        let path = match item.file.as_deref() {
            None => Err("no exported file name in the manifest"),
            Some(rel) => resolve_file(dir, rel),
        };
        let path = match path {
            Ok(p) => p,
            Err(reason) => {
                fail(&mut report, item.pk, reason);
                continue;
            }
        };

        if dry_run {
            let Ok(sha) = sha256_of(&path) else {
                fail(&mut report, item.pk, "the exported file cannot be read");
                continue;
            };
            let known: Option<(String,)> =
                sqlx::query_as("SELECT sha256 FROM files WHERE sha256 = ?")
                    .bind(&sha)
                    .fetch_optional(&app.db.read)
                    .await?;
            if known.is_some() || !planned.insert(sha) {
                report.duplicates += 1;
            } else {
                report.imported += 1;
                vocab.plan(item, &mut report);
            }
            continue;
        }

        let Ok(file) = tokio::fs::File::open(&path).await else {
            fail(&mut report, item.pk, "the exported file cannot be read");
            continue;
        };
        let original_name = item.original_name.clone().unwrap_or_else(|| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let input = UploadInput {
            original_name,
            declared_mime: item.mime.clone(),
            ..UploadInput::default()
        };
        let id = match upload(&ctx, input, file).await {
            Ok(UploadOutcome::Created(id)) => id,
            Ok(UploadOutcome::Restored(_)) => {
                report.restored += 1;
                continue;
            }
            Err(e) if e.code == "duplicate" => {
                report.duplicates += 1;
                continue;
            }
            Err(e) => {
                fail(
                    &mut report,
                    item.pk,
                    &format!("upload refused ({})", e.code),
                );
                continue;
            }
        };
        report.imported += 1;

        let (tags, org, mut incomplete) = vocab.resolve(app, actor, item, &mut report).await;
        let patch = DocumentPatch {
            title: item.title.clone().map(Some),
            document_date: item.date.map(Some),
            organizations: org.map(|id| {
                vec![OrganizationLink {
                    id,
                    role: Role::Unspecified,
                }]
            }),
            tags: (!tags.is_empty()).then_some(tags),
            ..DocumentPatch::default()
        };
        if let Err(e) = documents::update_document(&ctx, id, patch).await {
            fail(
                &mut report,
                item.pk,
                &format!("stored, but its metadata was not applied ({})", e.code),
            );
            incomplete = false;
        }
        if incomplete {
            fail(
                &mut report,
                item.pk,
                "stored, but a tag or the correspondent could not be created",
            );
        }
    }
    Ok(report)
}

fn fail(report: &mut Report, pk: i64, reason: &str) {
    if report.failed.len() < MAX_REASONS {
        report.failed.push((pk, reason.to_string()));
    }
}
