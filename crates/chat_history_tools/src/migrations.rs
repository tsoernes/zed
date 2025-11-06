use anyhow::{anyhow, Context, Result};
use sea_orm::ConnectionTrait;
use sea_orm::DatabaseConnection;
use std::collections::hash_map::DefaultHasher;
use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

/// Represents an applied migration (loaded from the tracking table).
#[derive(Debug, Clone)]
pub struct AppliedMigration {
    pub version: String,
    pub checksum: String,
}

/// Apply all chat history migrations located in `migrations_dir` against `conn`.
/// Returns a vector of (version, duration_ms) for migrations applied in this invocation.
///
/// This uses a dedicated tracking table `chat_history_migrations` so it does not
/// interfere with application-wide migrations in other subsystems.
///
/// Each migration file is executed exactly once. If a file's checksum changes
/// after being applied previously, an error is returned to surface potential
/// manual tampering.
///
/// File naming convention is lexicographic ordering:
///     20250101000000_chat_history.sql
/// The entire file stem (without extension) is treated as the version identifier.
///
/// A lightweight checksum (DefaultHasher u64) is used to detect changes without
/// introducing a new crypto dependency in this crate.
pub async fn run_chat_history_migrations(
    conn: &DatabaseConnection,
    migrations_dir: impl AsRef<Path>,
) -> Result<Vec<(String, u128)>> {
    ensure_tracking_table(conn).await?;

    let migrations = discover_migration_files(migrations_dir.as_ref())?;
    let applied = load_applied_migrations(conn).await?;

    let mut newly_applied = Vec::new();

    for file in migrations {
        let version = file
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow!("invalid migration file name: {:?}", file))?
            .to_string();

        let sql = fs::read_to_string(&file)
            .with_context(|| format!("failed to read migration file {:?}", file))?;
        let checksum = checksum_str(&sql);

        if let Some(existing) = applied.iter().find(|m| m.version == version) {
            if existing.checksum != checksum {
                return Err(anyhow!(
                    "checksum mismatch for already applied chat history migration {version}"
                ));
            }
            continue; // Already applied; skip.
        }

        let started = std::time::Instant::now();
        conn.execute_unprepared(&sql)
            .await
            .with_context(|| format!("failed executing migration {version}"))?;
        record_migration(conn, &version, &checksum).await?;
        let elapsed = started.elapsed().as_millis();
        newly_applied.push((version, elapsed));
    }

    Ok(newly_applied)
}

/// Create tracking table if it does not exist.
async fn ensure_tracking_table(conn: &DatabaseConnection) -> Result<()> {
    conn.execute_unprepared(
        r#"
        CREATE TABLE IF NOT EXISTS chat_history_migrations (
            version TEXT PRIMARY KEY,
            checksum TEXT NOT NULL,
            applied_at TEXT NOT NULL
        );
        "#,
    )
    .await
    .context("creating chat_history_migrations tracking table")?;
    Ok(())
}

/// Load applied migrations from tracking table.
async fn load_applied_migrations(conn: &DatabaseConnection) -> Result<Vec<AppliedMigration>> {
    // Use a minimal select without SeaORM entities to avoid generating extra models.
    let stmt = sea_orm::Statement::from_string(
        conn.get_database_backend(),
        "SELECT version, checksum FROM chat_history_migrations ORDER BY version".to_string(),
    );
    let rows = conn
        .query_all(stmt)
        .await
        .context("query applied chat history migrations")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let version = row
            .try_get::<String>("", "version")
            .context("reading version column")?;
        let checksum = row
            .try_get::<String>("", "checksum")
            .context("reading checksum column")?;
        out.push(AppliedMigration { version, checksum });
    }
    Ok(out)
}

/// Record a newly applied migration in tracking table.
async fn record_migration(conn: &DatabaseConnection, version: &str, checksum: &str) -> Result<()> {
    let applied_at = OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    let stmt = sea_orm::Statement::from_string(
        conn.get_database_backend(),
        format!(
            "INSERT INTO chat_history_migrations (version, checksum, applied_at) VALUES ('{}','{}','{}')",
            escape_single_quotes(version),
            escape_single_quotes(checksum),
            applied_at
        ),
    );
    conn.execute(stmt)
        .await
        .with_context(|| format!("inserting migration record {version}"))?;
    Ok(())
}

/// Escape single quotes for safe interpolation.
fn escape_single_quotes(input: &str) -> String {
    input.replace('\'', "''")
}

/// Discover .sql migration files in directory (non-recursive), sorted lexicographically.
fn discover_migration_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("reading chat history migrations dir {:?}", dir))?
    {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .and_then(OsStr::to_str)
            .map(|ext| ext.eq_ignore_ascii_case("sql"))
            .unwrap_or(false)
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Simple non-cryptographic checksum used to detect accidental edits.
fn checksum_str(sql: &str) -> String {
    let mut hasher = DefaultHasher::new();
    sql.hash(&mut hasher);
    let value = hasher.finish();
    format!("{value:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::Database;

    #[tokio::test]
    async fn applies_and_skips_second_run() -> Result<()> {
        let conn = Database::connect("sqlite::memory:").await?;
        // Create a temporary directory structure in memory by writing to OS temp.
        let temp = tempfile::tempdir()?;
        let file_path = temp.path().join("20250101000000_chat_history.sql");
        std::fs::write(
            &file_path,
            r#"
            CREATE TABLE chats (id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
            "#,
        )?;
        let first = run_chat_history_migrations(&conn, temp.path()).await?;
        assert_eq!(first.len(), 1, "expected one migration applied first run");
        let second = run_chat_history_migrations(&conn, temp.path()).await?;
        assert!(second.is_empty(), "second run should apply nothing");
        Ok(())
    }

    #[tokio::test]
    async fn checksum_mismatch_errors() -> Result<()> {
        let conn = Database::connect("sqlite::memory:").await?;
        let temp = tempfile::tempdir()?;
        let file_path = temp.path().join("20250101000000_chat_history.sql");
        std::fs::write(
            &file_path,
            "CREATE TABLE chats (id TEXT PRIMARY KEY, created_at TEXT NOT NULL);",
        )?;
        let _ = run_chat_history_migrations(&conn, temp.path()).await?;

        // Modify file contents (simulate tampering).
        std::fs::write(
            &file_path,
            "CREATE TABLE chats (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, modified TEXT);",
        )?;

        let err = run_chat_history_migrations(&conn, temp.path())
            .await
            .expect_err("expected checksum mismatch error");
        assert!(
            err.to_string()
                .contains("checksum mismatch for already applied chat history migration"),
            "unexpected error message: {err}"
        );
        Ok(())
    }
}
