//! Background removal of expired boxes.

use std::time::Duration;

use crate::{AppState, db, unix_now};

/// Deletes expired boxes every `interval` for as long as the server runs.
pub fn spawn(state: AppState, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match sweep(&state).await {
                Ok(0) => {}
                Ok(count) => tracing::info!(files = count, "removed expired files"),
                Err(error) => tracing::error!(%error, "cleanup failed"),
            }
        }
    });
}

/// Deletes every expired box and its files, returning how many files were removed.
pub async fn sweep(state: &AppState) -> Result<usize, sqlx::Error> {
    let file_ids = db::delete_expired(&state.db, unix_now()).await?;
    for id in &file_ids {
        state.storage.remove(id).await;
    }
    Ok(file_ids.len())
}
