//! On-disk blob store. Files are saved under random ids, never under the names
//! clients send, so uploads cannot escape the storage directory.

use std::io;
use std::path::{Path, PathBuf};

use actix_web::web::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("the size limit was exceeded")]
    TooLarge,
    #[error("upload interrupted: {0}")]
    Payload(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone)]
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).await?;
        Ok(Self { root })
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
        let result = write_limited(&path, body, limit).await;
        if result.is_err() {
            remove_quietly(&path).await;
        }
        result
    }

    pub async fn remove(&self, id: &str) {
        remove_quietly(&self.path(id)).await;
    }
}

async fn write_limited<S, E>(path: &Path, mut body: S, limit: u64) -> Result<u64, WriteError>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await?;
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
}
