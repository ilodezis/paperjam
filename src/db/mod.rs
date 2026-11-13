//! SQLite access: one writer connection, a small read pool, embedded migrations.

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

const WRITE_CONNECTIONS: u32 = 1;
const READ_CONNECTIONS: u32 = 4;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The embedded migrations, applied by [`Db::open`].
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("src/db/migrations");

pub struct Db {
    /// Single writer connection: serializes all writes.
    pub write: SqlitePool,
    /// Read-only connections for queries that do not mutate.
    pub read: SqlitePool,
}

impl Db {
    /// Opens (creating if needed) the database at `path` and applies migrations.
    pub async fn open(path: &Path) -> anyhow::Result<Db> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);

        let write = SqlitePoolOptions::new()
            .max_connections(WRITE_CONNECTIONS)
            .connect_with(options.clone())
            .await?;
        MIGRATOR.run(&write).await?;

        // Opened after the writer so the file and its WAL exist.
        let read = SqlitePoolOptions::new()
            .max_connections(READ_CONNECTIONS)
            .connect_with(options.read_only(true))
            .await?;

        Ok(Db { write, read })
    }

    /// Forces every commit so far onto disk. The writer runs with
    /// `synchronous=NORMAL`, where a WAL commit is not fsynced; a full
    /// checkpoint fsyncs the WAL, copies it into the database file and fsyncs
    /// that too. For callers about to destroy the only other copy of the data.
    pub async fn flush_durable(&self) -> Result<(), sqlx::Error> {
        let (busy, _, _): (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(FULL)")
            .fetch_one(&self.write)
            .await?;
        if busy != 0 {
            return Err(sqlx::Error::Protocol(
                "the WAL checkpoint could not complete".into(),
            ));
        }
        Ok(())
    }

    /// Cheap liveness check used by `/healthz`.
    pub async fn ping(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1")
            .execute(&self.read)
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::PersonId;

    async fn open_temp() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("paperjam.db")).await.unwrap();
        (dir, db)
    }

    /// `sqlite_master` SQL for every table and index, sorted by name.
    async fn schema_dump(db: &Db) -> String {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT sql FROM sqlite_master \
             WHERE sql IS NOT NULL AND name NOT LIKE '\\_sqlx%' ESCAPE '\\' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             ORDER BY name",
        )
        .fetch_all(&db.read)
        .await
        .unwrap();
        rows.into_iter()
            .map(|(sql,)| format!("{};\n", sql.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn migrations_apply_on_empty_db() {
        let (_dir, db) = open_temp().await;
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(&db.read)
                .await
                .unwrap();
        let names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();
        for expected in [
            "users",
            "api_tokens",
            "sessions",
            "people",
            "correspondents",
            "tags",
            "document_types",
            "documents",
            "document_people",
            "document_tags",
            "document_organizations",
            "files",
            "pages",
            "text_layers",
            "field_values",
            "facts",
            "llm_usage",
            "events",
            "jobs",
            "oauth_clients",
            "oauth_grants",
            "upload_links",
            "passkeys",
        ] {
            assert!(names.contains(&expected), "missing table {expected}");
        }
    }

    #[tokio::test]
    async fn schema_matches_snapshot() {
        let (_dir, db) = open_temp().await;
        let dump = schema_dump(&db).await;
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db/schema.txt");
        if std::env::var_os("UPDATE_SCHEMA").is_some() {
            std::fs::write(&path, &dump).unwrap();
        }
        let snapshot = std::fs::read_to_string(&path)
            .expect("src/db/schema.txt missing; run with UPDATE_SCHEMA=1")
            .replace("\r\n", "\n");
        assert_eq!(
            dump, snapshot,
            "schema changed; review the diff and run with UPDATE_SCHEMA=1"
        );
    }

    #[tokio::test]
    async fn pragmas_are_applied() {
        let (_dir, db) = open_temp().await;
        let (fk,): (i64,) = sqlx::query_as("PRAGMA foreign_keys")
            .fetch_one(&db.write)
            .await
            .unwrap();
        assert_eq!(fk, 1);
        let (mode,): (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&db.write)
            .await
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn flush_durable_checkpoints_the_wal() {
        let (_dir, db) = open_temp().await;
        sqlx::query("CREATE TABLE durable_probe (x INTEGER)")
            .execute(&db.write)
            .await
            .unwrap();
        db.flush_durable().await.unwrap();
    }

    #[tokio::test]
    async fn read_pool_cannot_write() {
        let (_dir, db) = open_temp().await;
        let res = sqlx::query("INSERT INTO tags (id, name) VALUES ('t', 'x')")
            .execute(&db.read)
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn ids_roundtrip_through_sqlite() {
        let (_dir, db) = open_temp().await;
        let id = PersonId::new();
        sqlx::query(
            "INSERT INTO people (id, name, aliases, notes, created_at) VALUES (?, 'A', '[]', '', ?)",
        )
        .bind(id)
        .bind("2026-01-01T00:00:00Z")
        .execute(&db.write)
        .await
        .unwrap();
        let (back,): (PersonId,) = sqlx::query_as("SELECT id FROM people")
            .fetch_one(&db.read)
            .await
            .unwrap();
        assert_eq!(back, id);
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced() {
        let (_dir, db) = open_temp().await;
        let res = sqlx::query(
            "INSERT INTO api_tokens (id, user_id, name, token_hash, prefix, created_at) \
             VALUES ('t', 'no-such-user', 'n', 'h', 'p', '2026-01-01T00:00:00Z')",
        )
        .execute(&db.write)
        .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn migration_0006_backfills_document_organizations() {
        use sqlx::Executor;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("pre0006.db");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();

        // Apply migrations up to 0005
        for m in MIGRATOR.iter() {
            if m.version <= 5 {
                pool.execute(m.sql.as_ref()).await.unwrap();
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS _sqlx_migrations ( \
                     version BIGINT PRIMARY KEY, \
                     description TEXT NOT NULL, \
                     installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
                     success BOOLEAN NOT NULL, \
                     checksum BLOB NOT NULL, \
                     execution_time BIGINT NOT NULL)",
                )
                .execute(&pool)
                .await
                .unwrap();
                sqlx::query(
                    "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                     VALUES (?, ?, 1, ?, 0)",
                )
                .bind(m.version)
                .bind(&m.description)
                .bind(&m.checksum)
                .execute(&pool)
                .await
                .unwrap();
            }
        }

        // Insert correspondent and documents with old columns
        sqlx::query(
            "INSERT INTO correspondents (id, name, aliases, created_at) \
             VALUES ('c1', 'Old Corp', '[]', '2026-01-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO documents (id, title, correspondent_id, correspondent_provenance, created_at, updated_at, local_only) \
             VALUES ('d1', 'Contract', 'c1', 'llm', '2026-01-01', '2026-01-01', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Document with NULL provenance defaults to 'local'
        sqlx::query(
            "INSERT INTO documents (id, title, correspondent_id, correspondent_provenance, created_at, updated_at, local_only) \
             VALUES ('d2', 'Invoice', 'c1', NULL, '2026-01-01', '2026-01-01', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Document with no correspondent
        sqlx::query(
            "INSERT INTO documents (id, title, correspondent_id, correspondent_provenance, created_at, updated_at, local_only) \
             VALUES ('d3', 'Receipt', NULL, NULL, '2026-01-01', '2026-01-01', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Now run MIGRATOR to apply 0006
        MIGRATOR.run(&pool).await.unwrap();

        // Verify d1
        let row1: (String, String, String, String) = sqlx::query_as(
            "SELECT document_id, correspondent_id, role, provenance FROM document_organizations WHERE document_id = 'd1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row1,
            ("d1".into(), "c1".into(), "unspecified".into(), "llm".into())
        );

        // Verify d2 (provenance was NULL -> 'local')
        let row2: (String, String, String, String) = sqlx::query_as(
            "SELECT document_id, correspondent_id, role, provenance FROM document_organizations WHERE document_id = 'd2'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            row2,
            (
                "d2".into(),
                "c1".into(),
                "unspecified".into(),
                "local".into()
            )
        );

        // Verify d3 (no row)
        let row3: Option<(String,)> = sqlx::query_as(
            "SELECT document_id FROM document_organizations WHERE document_id = 'd3'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(row3.is_none());

        // Verify old columns on documents are all NULL
        let old_cols: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT correspondent_id, correspondent_provenance FROM documents ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        for (cid, prov) in old_cols {
            assert!(cid.is_none());
            assert!(prov.is_none());
        }

        // Verify index documents_correspondent was dropped
        let idx: Option<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'documents_correspondent'",
        )
        .fetch_optional(&pool)
        .await
        .unwrap();
        assert!(idx.is_none());
    }

    #[tokio::test]
    async fn migration_0008_flags_the_known_issuer_fields_of_stored_types() {
        use sqlx::Executor;

        let dir = tempfile::tempdir().unwrap();
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("pre0008.db"))
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        pool.execute(
            "CREATE TABLE _sqlx_migrations ( \
             version BIGINT PRIMARY KEY, description TEXT NOT NULL, \
             installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, \
             success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)",
        )
        .await
        .unwrap();
        for m in MIGRATOR.iter().filter(|m| m.version <= 7) {
            pool.execute(m.sql.as_ref()).await.unwrap();
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
                 VALUES (?, ?, 1, ?, 0)",
            )
            .bind(m.version)
            .bind(&m.description)
            .bind(&m.checksum)
            .execute(&pool)
            .await
            .unwrap();
        }
        let legacy = [
            // A built-in type: the issuer field is flagged, the rest is kept.
            (
                "passport",
                1,
                r#"[{"key":"number","labels":{"en":"N"},"kind":"identifier","required":true},{"key":"issuing_authority","labels":{"en":"I"},"kind":"text","required":false}]"#,
            ),
            // A user type with a key from the old list on a text field, and
            // the same key on a field that is not text.
            (
                "mine",
                0,
                r#"[{"key":"issuer","labels":{"en":"I"},"kind":"text"},{"key":"bank","labels":{"en":"B"},"kind":"identifier"}]"#,
            ),
            ("empty", 0, "[]"),
        ];
        for (key, builtin, fields) in legacy {
            sqlx::query(
                "INSERT INTO document_types (id, key, labels, fields, builtin, created_at) \
                 VALUES (?, ?, '{\"en\":\"T\"}', ?, ?, '2026-01-01')",
            )
            .bind(key)
            .bind(key)
            .bind(fields)
            .bind(builtin)
            .execute(&pool)
            .await
            .unwrap();
        }

        MIGRATOR.run(&pool).await.unwrap();

        let fields_of = |key: &'static str| {
            let pool = pool.clone();
            async move {
                let json: String =
                    sqlx::query_scalar("SELECT fields FROM document_types WHERE key = ?")
                        .bind(key)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                serde_json::from_str::<serde_json::Value>(&json).unwrap()
            }
        };
        let passport = fields_of("passport").await;
        assert_eq!(passport[0]["key"], "number", "field order is kept");
        assert_eq!(passport[0]["required"], true);
        assert!(passport[0].get("issuer").is_none());
        assert_eq!(passport[1]["issuer"], true);
        let mine = fields_of("mine").await;
        assert_eq!(mine[0]["issuer"], true, "a user type keeps its fallback");
        assert!(
            mine[1].get("issuer").is_none(),
            "only text fields are flagged"
        );
        assert_eq!(fields_of("empty").await, serde_json::json!([]));
    }
}
