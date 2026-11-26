//! Turns a multipart request into stored files.

use std::time::Duration;

use actix_multipart::{Field, Multipart};
use futures_util::{StreamExt, TryStreamExt};

use crate::AppState;
use crate::db::FileRecord;
use crate::error::AppError;
use crate::storage::WriteError;
use crate::token;

const FILE_FIELD: &str = "file";
const EXPIRES_FIELD: &str = "expires";
const MAX_TEXT_FIELD_LEN: usize = 64;
const MAX_NAME_LEN: usize = 255;

/// Everything a client sent in one upload request.
pub struct Upload {
    pub files: Vec<FileRecord>,
    pub expiry: Option<Duration>,
}

/// Reads the multipart body, saving each `file` field to storage. On failure,
/// files saved so far are removed again so nothing is left behind.
pub async fn receive(state: &AppState, multipart: Multipart) -> Result<Upload, AppError> {
    let mut upload = Upload {
        files: Vec::new(),
        expiry: None,
    };
    match read_fields(state, multipart, &mut upload).await {
        Ok(()) => Ok(upload),
        Err(error) => {
            discard(state, &upload.files).await;
            Err(error)
        }
    }
}

/// Removes stored files that will not be kept.
pub async fn discard(state: &AppState, files: &[FileRecord]) {
    for file in files {
        state.storage.remove(&file.id).await;
    }
}

async fn read_fields(
    state: &AppState,
    mut multipart: Multipart,
    upload: &mut Upload,
) -> Result<(), AppError> {
    let config = &state.config;
    let limit = config.max_upload_size.as_u64();
    let mut used: u64 = 0;

    while let Some(field) = multipart.next().await {
        let field = field.map_err(|error| AppError::BadRequest(error.to_string()))?;

        match field.name() {
            Some(FILE_FIELD) => {
                if upload.files.len() == config.max_files {
                    return Err(AppError::BadRequest(format!(
                        "too many files: a box holds at most {}",
                        config.max_files
                    )));
                }
                let file = store_file(state, field, limit - used).await?;
                used += file.size as u64;
                upload.files.push(file);
            }
            Some(EXPIRES_FIELD) => {
                let text = read_text(field).await?;
                upload.expiry = Some(parse_expiry(&text, |expiry| config.accepts_expiry(expiry))?);
            }
            other => {
                return Err(AppError::BadRequest(format!(
                    "unexpected form field `{}`",
                    other.unwrap_or_default()
                )));
            }
        }
    }
    Ok(())
}

async fn store_file(state: &AppState, field: Field, remaining: u64) -> Result<FileRecord, AppError> {
    let name = display_name(field.content_disposition().and_then(|cd| cd.get_filename()));
    let content_type = mime_guess::from_path(&name).first_or_octet_stream().to_string();
    let id = token::file_id();

    let size = state
        .storage
        .write(&id, field, remaining)
        .await
        .map_err(|error| match error {
            WriteError::TooLarge => AppError::TooLarge(state.config.max_upload_size_label()),
            WriteError::Payload(message) => AppError::BadRequest(message),
            WriteError::Io(error) => AppError::from(error),
        })?;

    Ok(FileRecord {
        id,
        name,
        size: size as i64,
        content_type,
    })
}

async fn read_text(field: Field) -> Result<String, AppError> {
    let bytes = field
        .map_err(|error| AppError::BadRequest(error.to_string()))
        .try_fold(Vec::new(), |mut bytes, chunk| async move {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_TEXT_FIELD_LEN {
                return Err(AppError::BadRequest("form field is too long".into()));
            }
            Ok(bytes)
        })
        .await?;
    String::from_utf8(bytes).map_err(|_| AppError::BadRequest("form field is not valid UTF-8".into()))
}

fn parse_expiry(text: &str, accepts: impl Fn(Duration) -> bool) -> Result<Duration, AppError> {
    humantime::parse_duration(text.trim())
        .ok()
        .filter(|&expiry| accepts(expiry))
        .ok_or_else(|| AppError::BadRequest(format!("`{}` is not an allowed expiry", text.trim())))
}

/// Reduces a client-supplied file name to a safe, readable base name.
/// The result is only ever shown to people; it never becomes a path on disk.
fn display_name(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or_default()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_LEN)
        .collect();
    let cleaned = cleaned.trim();

    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "unnamed".into()
    } else {
        cleaned.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_strips_paths_and_controls() {
        assert_eq!(display_name(Some("../../etc/passwd")), "passwd");
        assert_eq!(display_name(Some(r"C:\Users\me\photo.png")), "photo.png");
        assert_eq!(display_name(Some("re\u{0}port\n.pdf")), "report.pdf");
        assert_eq!(display_name(Some("  café.txt ")), "café.txt");
    }

    #[test]
    fn display_name_falls_back_when_nothing_is_left() {
        assert_eq!(display_name(None), "unnamed");
        assert_eq!(display_name(Some("dir/")), "unnamed");
        assert_eq!(display_name(Some("..")), "unnamed");
    }

    #[test]
    fn display_name_is_length_limited() {
        assert_eq!(display_name(Some(&"é".repeat(400))).chars().count(), MAX_NAME_LEN);
    }

    #[test]
    fn parses_expiry_within_bounds() {
        let up_to_a_day = |expiry: Duration| expiry <= Duration::from_secs(86_400);
        assert_eq!(
            parse_expiry(" 1h ", up_to_a_day).unwrap(),
            Duration::from_secs(3600)
        );
        assert!(parse_expiry("7d", up_to_a_day).is_err());
        assert!(parse_expiry("soon", up_to_a_day).is_err());
    }
}
