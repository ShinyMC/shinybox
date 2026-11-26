//! On-disk blob store. Files are saved under random ids, never under the names
//! clients send, so uploads cannot escape the storage directory.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use actix_web::web::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::fs::{self, File};
use tokio::io::AsyncWriteExt;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("the size limit was exceeded")]
    TooLarge,
    #[error("upload interrupted: {0}")]
    Payload(String),
    #[error("the file holds {0} bytes, not the given offset")]
    OffsetMismatch(u64),
    #[error("another upload to this file is in progress")]
    Busy,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
    /// Files currently being appended to, so two requests never write the same file at once.
    appending: Arc<Mutex<HashSet<String>>>,
}

impl Storage {
    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).await?;
        Ok(Self {
            root,
            appending: Arc::default(),
        })
    }

    pub fn path(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Streams `body` into a new file and returns its size. Fails once more than
    /// `limit` bytes arrive; a partially written file is removed on any failure.
    pub async fn write<S, E>(&self, id: &str, body: S, limit: u64) -> Result<u64, WriteError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let path = self.path(id);
        let result = async {
            let mut file = create_new(&path).await?;
            copy_limited(&mut file, body, limit).await
        }
        .await;
        if result.is_err() {
            remove_quietly(&path).await;
        }
        result
    }

    /// Creates an empty file that chunks are appended to later.
    pub async fn create(&self, id: &str) -> io::Result<()> {
        create_new(&self.path(id)).await.map(drop)
    }

    /// Appends `body` to a file that currently holds exactly `offset` bytes and returns
    /// its new size. On failure the file is cut back to `offset`, so the same chunk can be
    /// sent again.
    pub async fn append<S, E>(&self, id: &str, offset: u64, body: S, limit: u64) -> Result<u64, WriteError>
    where
        S: Stream<Item = Result<Bytes, E>> + Unpin,
        E: std::fmt::Display,
    {
        let _guard = AppendGuard::acquire(&self.appending, id).ok_or(WriteError::Busy)?;
        let mut file = fs::OpenOptions::new().append(true).open(self.path(id)).await?;
        let size = file.metadata().await?.len();
        if size != offset {
            return Err(WriteError::OffsetMismatch(size));
        }

        match copy_limited(&mut file, body, limit).await {
            Ok(written) => Ok(offset + written),
            Err(error) => {
                if let Err(truncate_error) = file.set_len(offset).await {
                    tracing::warn!(id, error = %truncate_error, "could not undo a failed append");
                }
                Err(error)
            }
        }
    }

    /// Current size of a stored file.
    pub async fn size(&self, id: &str) -> io::Result<u64> {
        Ok(fs::metadata(self.path(id)).await?.len())
    }

    pub async fn remove(&self, id: &str) {
        remove_quietly(&self.path(id)).await;
    }
}

/// Marks one file as being appended to for as long as it is alive.
struct AppendGuard<'a> {
    appending: &'a Mutex<HashSet<String>>,
    id: String,
}

impl<'a> AppendGuard<'a> {
    fn acquire(appending: &'a Mutex<HashSet<String>>, id: &str) -> Option<Self> {
        let inserted = appending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned());
        inserted.then(|| Self {
            appending,
            id: id.to_owned(),
        })
    }
}

impl Drop for AppendGuard<'_> {
    fn drop(&mut self) {
        self.appending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

async fn create_new(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
}

/// Copies `body` into `file`, failing as soon as more than `limit` bytes arrive.
async fn copy_limited<S, E>(file: &mut File, mut body: S, limit: u64) -> Result<u64, WriteError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut written: u64 = 0;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| WriteError::Payload(error.to_string()))?;
        written += chunk.len() as u64;
        if written > limit {
            return Err(WriteError::TooLarge);
        }
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    Ok(written)
}

async fn remove_quietly(path: &Path) {
    if let Err(error) = fs::remove_file(path).await
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "could not remove stored file");
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use futures_util::stream;

    use super::*;

    fn body(chunks: &[&'static str]) -> impl Stream<Item = Result<Bytes, Infallible>> + Unpin {
        stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok(Bytes::from_static(chunk.as_bytes()))),
        )
    }

    #[tokio::test]
    async fn writes_within_limit() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let size = storage.write("a", body(&["hello ", "world"]), 11).await.unwrap();

        assert_eq!(size, 11);
        assert_eq!(std::fs::read_to_string(storage.path("a")).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn removes_partial_file_over_limit() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();

        let result = storage.write("a", body(&["hello ", "world"]), 10).await;

        assert!(matches!(result, Err(WriteError::TooLarge)));
        assert!(!storage.path("a").exists());
    }

    #[tokio::test]
    async fn appends_at_the_expected_offset() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage.create("a").await.unwrap();

        assert_eq!(storage.append("a", 0, body(&["hello "]), 100).await.unwrap(), 6);
        assert_eq!(storage.append("a", 6, body(&["world"]), 100).await.unwrap(), 11);
        assert!(matches!(
            storage.append("a", 6, body(&["again"]), 100).await,
            Err(WriteError::OffsetMismatch(11))
        ));
        assert_eq!(std::fs::read_to_string(storage.path("a")).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn failed_append_restores_the_previous_size() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).await.unwrap();
        storage.create("a").await.unwrap();
        storage.append("a", 0, body(&["hello"]), 100).await.unwrap();

        let result = storage.append("a", 5, body(&[" big", " world"]), 6).await;

        assert!(matches!(result, Err(WriteError::TooLarge)));
        assert_eq!(storage.size("a").await.unwrap(), 5);
    }

    #[test]
    fn one_append_per_file_at_a_time() {
        let appending = Mutex::default();
        let first = AppendGuard::acquire(&appending, "a");
        assert!(first.is_some());
        assert!(AppendGuard::acquire(&appending, "a").is_none());
        assert!(AppendGuard::acquire(&appending, "b").is_some());
        drop(first);
        assert!(AppendGuard::acquire(&appending, "a").is_some());
    }
}
