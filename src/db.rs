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
}

pub async fn insert_box(pool: &SqlitePool, new: &NewBox<'_>) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query("INSERT INTO boxes (code, delete_token_hash, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(&new.record.code)
        .bind(&new.delete_token_hash)
        .bind(new.record.created_at)
        .bind(new.record.expires_at)
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

/// Finds a box that has not expired yet.
pub async fn find_box(pool: &SqlitePool, code: &str, now: i64) -> Result<Option<BoxRecord>, sqlx::Error> {
    sqlx::query_as("SELECT code, created_at, expires_at FROM boxes WHERE code = ? AND expires_at > ?")
        .bind(code)
        .bind(now)
        .fetch_optional(pool)
        .await
}

pub async fn box_files(pool: &SqlitePool, code: &str) -> Result<Vec<FileRecord>, sqlx::Error> {
    sqlx::query_as("SELECT id, name, size, content_type FROM files WHERE box_code = ? ORDER BY position")
        .bind(code)
        .fetch_all(pool)
        .await
}

/// Finds a file inside a box that has not expired yet.
pub async fn find_file(
    pool: &SqlitePool,
    code: &str,
    file_id: &str,
    now: i64,
) -> Result<Option<FileRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT f.id, f.name, f.size, f.content_type
         FROM files f JOIN boxes b ON b.code = f.box_code
         WHERE f.box_code = ? AND f.id = ? AND b.expires_at > ?",
    )
    .bind(code)
    .bind(file_id)
    .bind(now)
    .fetch_optional(pool)
    .await
}

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
