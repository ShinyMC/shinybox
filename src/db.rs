//! SQLite persistence for boxes and the files they contain.
//! Timestamps are Unix seconds.

use std::path::Path;

use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

pub async fn open(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new().connect_with(options).await?;
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BoxRecord {
    pub code: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FileRecord {
    pub id: String,
    pub name: String,
    pub size: i64,
    pub content_type: String,
}

pub struct NewBox<'a> {
    pub record: BoxRecord,
    pub delete_token_hash: String,
    pub files: &'a [FileRecord],
    /// Set for a box whose files are still being uploaded: its lifetime in seconds once complete.
    pub pending_lifetime: Option<i64>,
}

/// A box created through the resumable upload API whose files have not all arrived yet.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PendingBox {
    pub code: String,
    pub delete_token_hash: String,
    pub pending_lifetime: i64,
}

pub async fn insert_box(pool: &SqlitePool, new: &NewBox<'_>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "INSERT INTO boxes (code, delete_token_hash, created_at, expires_at, pending_lifetime)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&new.record.code)
    .bind(&new.delete_token_hash)
    .bind(new.record.created_at)
    .bind(new.record.expires_at)
    .bind(new.pending_lifetime)
    .execute(&mut *tx)
    .await?;

    for (position, file) in (0_i64..).zip(new.files) {
        sqlx::query(
            "INSERT INTO files (id, box_code, position, name, size, content_type) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&file.id)
        .bind(&new.record.code)
        .bind(position)
        .bind(&file.name)
        .bind(file.size)
        .bind(&file.content_type)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await
}

/// Finds a complete box that has not expired yet.
pub async fn find_box(pool: &SqlitePool, code: &str, now: i64) -> Result<Option<BoxRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT code, created_at, expires_at FROM boxes
         WHERE code = ? AND expires_at > ? AND pending_lifetime IS NULL",
    )
    .bind(code)
    .bind(now)
    .fetch_optional(pool)
    .await
}

/// Finds a box that is still receiving files and whose upload window is open.
pub async fn find_pending_box(
    pool: &SqlitePool,
    code: &str,
    now: i64,
) -> Result<Option<PendingBox>, sqlx::Error> {
    sqlx::query_as(
        "SELECT code, delete_token_hash, pending_lifetime FROM boxes
         WHERE code = ? AND expires_at > ? AND pending_lifetime IS NOT NULL",
    )
    .bind(code)
    .bind(now)
    .fetch_optional(pool)
    .await
}

/// Publishes a pending box. Returns false when it was not pending (any more).
pub async fn complete_box(pool: &SqlitePool, record: &BoxRecord) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE boxes SET created_at = ?, expires_at = ?, pending_lifetime = NULL
         WHERE code = ? AND pending_lifetime IS NOT NULL",
    )
    .bind(record.created_at)
    .bind(record.expires_at)
    .bind(&record.code)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn box_files(pool: &SqlitePool, code: &str) -> Result<Vec<FileRecord>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, size, content_type FROM files WHERE box_code = ? ORDER BY position")
        .bind(code)
        .fetch_all(pool)
        .await
}

/// Finds a file inside a complete box that has not expired yet.
pub async fn find_file(
    pool: &SqlitePool,
    code: &str,
    file_id: &str,
    now: i64,
) -> Result<Option<FileRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT f.id, f.name, f.size, f.content_type
         FROM files f JOIN boxes b ON b.code = f.box_code
         WHERE f.box_code = ? AND f.id = ? AND b.expires_at > ? AND b.pending_lifetime IS NULL",
    )
    .bind(code)
    .bind(file_id)
    .bind(now)
    .fetch_optional(pool)
    .await
}

/// The delete token hash of a live box, pending or complete, so an unfinished upload can be cancelled.
pub async fn delete_token_hash(
    pool: &SqlitePool,
    code: &str,
    now: i64,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT delete_token_hash FROM boxes WHERE code = ? AND expires_at > ?")
        .bind(code)
        .bind(now)
        .fetch_optional(pool)
        .await
}

/// Deletes one box and returns the ids of its files so they can be removed from disk.
pub async fn delete_box(pool: &SqlitePool, code: &str) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let file_ids = sqlx::query_scalar("SELECT id FROM files WHERE box_code = ?")
        .bind(code)
        .fetch_all(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM boxes WHERE code = ?")
        .bind(code)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(file_ids)
}

/// Deletes every expired box and returns the ids of their files.
pub async fn delete_expired(pool: &SqlitePool, now: i64) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let file_ids = sqlx::query_scalar(
        "SELECT f.id FROM files f JOIN boxes b ON b.code = f.box_code WHERE b.expires_at <= ?",
    )
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM boxes WHERE expires_at <= ?")
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(file_ids)
}
