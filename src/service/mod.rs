//! The service layer: all business logic. REST, MCP and the CLI are thin
//! adapters over these functions.

pub mod backup;
pub mod calendar;
pub mod documents;
pub mod events;
pub mod export;
pub mod links;
pub mod naming;
pub mod paperless;
pub mod passkeys;
pub mod setup;
pub mod shares;
pub mod stats;
pub mod taxonomy;
pub mod tokens;
pub mod upload;
pub mod users;

use std::sync::Arc;

use crate::config::Config;
use crate::db::Db;
use crate::error::{Result, ServiceError};
use crate::files::{FileStore, HashLocks};
use crate::ids::UserId;
use crate::jobs::queue::JobQueue;
use crate::llm::LlmClient;
use crate::search::SearchIndex;

/// How a request reached the service layer (recorded in the audit log).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Web,
    Token,
    OAuth,
    Cli,
    System,
    Inbox,
    Email,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::Web => "web",
            Via::Token => "token",
            Via::OAuth => "oauth",
            Via::Cli => "cli",
            Via::System => "system",
            Via::Inbox => "inbox",
            Via::Email => "email",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub user_id: Option<UserId>,
    pub via: Via,
}

impl Actor {
    pub fn system() -> Self {
        Self {
            user_id: None,
            via: Via::System,
        }
    }

    pub fn cli() -> Self {
        Self {
            user_id: None,
            via: Via::Cli,
        }
    }

    pub fn user(user_id: UserId, via: Via) -> Self {
        Self {
            user_id: Some(user_id),
            via,
        }
    }

    /// Sign-in credentials (accounts, API tokens) are handed out only from a
    /// signed-in web session or the server's own CLI. A bearer token or an
    /// OAuth client never mints new access, so a leaked one cannot outlive its
    /// revocation or climb to a web session.
    pub fn require_session_or_cli(&self) -> Result<()> {
        match self.via {
            Via::Web | Via::Cli => Ok(()),
            _ => Err(ServiceError::forbidden(
                "session_required",
                "this action needs a signed-in web session",
            )),
        }
    }

    pub fn require_user(&self) -> Result<UserId> {
        self.user_id.ok_or_else(|| {
            ServiceError::unauthorized(
                "authentication_required",
                "this action needs a signed-in user",
            )
        })
    }
}

/// Everything the service layer shares.
pub struct App {
    pub config: Arc<Config>,
    pub db: Db,
    pub files: FileStore,
    pub locks: HashLocks,
    /// Derived data; rebuilt from the database by `paperjam reindex`.
    pub index: SearchIndex,
    pub jobs: JobQueue,
    /// `None` without an LLM API key: only local recognition runs.
    pub llm: Option<LlmClient>,
    /// WebAuthn relying party and pending passkey ceremonies.
    pub passkeys: passkeys::Passkeys,
}

impl App {
    /// Opens the data directory: database, file store and index.
    pub async fn open(config: Config) -> anyhow::Result<App> {
        std::fs::create_dir_all(&config.data_dir)?;
        let db = Db::open(&config.data_dir.join("paperjam.db")).await?;
        let files = FileStore::open(&config.data_dir)?;
        let config_dir = config.data_dir.clone();
        let index = SearchIndex::open(&config_dir.join("index"), &config.ocr_langs)?;
        let llm = LlmClient::from_config(&config);
        let passkeys = passkeys::Passkeys::from_config(&config);
        Ok(App {
            config: Arc::new(config),
            files,
            locks: HashLocks::default(),
            index,
            jobs: JobQueue::new(db.write.clone()),
            llm,
            passkeys,
            db,
        })
    }

    pub fn ctx(&self, actor: Actor) -> Ctx<'_> {
        Ctx { app: self, actor }
    }
}

pub struct Ctx<'a> {
    pub app: &'a App,
    pub actor: Actor,
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub const TEST_SECRET: &str = "test-secret-test-secret-test-secret-0123";

    pub async fn test_app() -> (tempfile::TempDir, App) {
        // The fixtures are English, Russian and Romanian.
        test_app_with_langs("eng+rus+ron").await
    }

    /// Like [`test_app`] with another recognition (and so stemming) language set.
    pub async fn test_app_with_langs(ocr_langs: &str) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().to_string_lossy().into_owned();
        let config = Config::from_lookup(|name| match name {
            "PAPERJAM_SECRET_KEY" => Some(TEST_SECRET.to_string()),
            "PAPERJAM_DATA_DIR" => Some(data_dir.clone()),
            "PAPERJAM_OCR_LANGS" => Some(ocr_langs.to_string()),
            _ => None,
        })
        .unwrap();
        let app = App::open(config).await.unwrap();
        (dir, app)
    }

    /// Inserts a bare `queued` document row and returns its id.
    pub async fn insert_document(app: &App) -> crate::ids::DocumentId {
        let id = crate::ids::DocumentId::new();
        let now = crate::time::now_ts();
        sqlx::query("INSERT INTO documents (id, created_at, updated_at) VALUES (?, ?, ?)")
            .bind(id)
            .bind(&now)
            .bind(&now)
            .execute(&app.db.write)
            .await
            .unwrap();
        id
    }
}
