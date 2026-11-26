//! Resumable uploads under `/api/uploads`, for files too large for one request.
//!
//! A client declares the files of a box up front, sends each file in chunks that
//! fit through any reverse proxy, and then completes the box. Until then the box
//! is invisible to everyone else. Every step after the first is authorised with
//! the box's delete token, so only the uploader can add to it or cancel it.

use std::time::Duration;

use actix_web::http::header;
use actix_web::{HttpRequest, HttpResponse, web};
use serde::{Deserialize, Serialize};

use super::api::{self, BoxView, DELETE_TOKEN_HEADER};
use crate::db::{self, BoxRecord, FileRecord, NewBox, PendingBox};
use crate::error::AppError;
use crate::storage::WriteError;
use crate::upload::{discard, display_name, parse_expiry};
use crate::{AppState, token, unix_now};

/// Chunk size clients are asked to use: well below common proxy body limits.
const CHUNK_SIZE: u64 = 32 * 1024 * 1024;
/// How long an unfinished upload may take before its box is swept.
const UPLOAD_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// Enough for the declarations of thousands of files.
const DECLARATION_LIMIT: usize = 1024 * 1024;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::resource("")
            .app_data(
                web::JsonConfig::default()
                    .limit(DECLARATION_LIMIT)
                    .error_handler(|error, _| AppError::BadRequest(error.to_string()).into()),
            )
            .route(web::post().to(start)),
    )
    .app_data(
        web::QueryConfig::default().error_handler(|error, _| AppError::BadRequest(error.to_string()).into()),
    )
    .route("/{code}", web::get().to(status))
    .route("/{code}/files/{file_id}", web::put().to(append))
    .route("/{code}/complete", web::post().to(complete));
}

#[derive(Deserialize)]
struct Declaration {
    files: Vec<DeclaredFile>,
    expires: Option<String>,
}

#[derive(Deserialize)]
struct DeclaredFile {
    name: String,
    size: u64,
}

#[derive(Deserialize)]
struct ChunkQuery {
    offset: u64,
}

#[derive(Serialize)]
struct Started {
    code: String,
    delete_token: String,
    chunk_size: u64,
    upload_expires_at: i64,
    files: Vec<FileRecord>,
}

#[derive(Serialize)]
struct Progress {
    files: Vec<FileProgress>,
}

#[derive(Serialize)]
struct FileProgress {
    #[serde(flatten)]
    file: FileRecord,
    received: u64,
}

#[derive(Serialize)]
struct Received {
    received: u64,
}

async fn start(
    state: web::Data<AppState>,
    request: HttpRequest,
    declaration: web::Json<Declaration>,
) -> Result<HttpResponse, AppError> {
    api::authorize_upload(&state, &request)?;
    let Declaration { files, expires } = declaration.into_inner();
    let config = &state.config;

    if files.is_empty() {
        return Err(AppError::BadRequest("no files were declared".into()));
    }
    if files.len() > config.max_files {
        return Err(AppError::BadRequest(format!(
            "too many files: a box holds at most {}",
            config.max_files
        )));
    }
    let total = files
        .iter()
        .try_fold(0_u64, |sum, file| sum.checked_add(file.size))
        .filter(|&total| total <= config.max_upload_size.as_u64());
    if total.is_none() {
        return Err(AppError::TooLarge(config.max_upload_size_label()));
    }
    let lifetime = match expires {
        Some(text) => parse_expiry(&text, |expiry| config.accepts_expiry(expiry))?,
        None => config.default_expiry,
    };

    let files: Vec<FileRecord> = files
        .into_iter()
        .map(|file| {
            let name = display_name(Some(&file.name));
            FileRecord {
                id: token::file_id(),
                content_type: mime_guess::from_path(&name).first_or_octet_stream().to_string(),
                name,
                size: file.size as i64,
            }
        })
        .collect();

    let now = unix_now();
    let upload_expires_at = now + UPLOAD_WINDOW.as_secs() as i64;
    let delete_token = token::secret();
    let new_box = NewBox {
        record: BoxRecord {
            code: token::share_code(),
            created_at: now,
            expires_at: upload_expires_at,
        },
        delete_token_hash: token::hash(&delete_token),
        files: &files,
        pending_lifetime: Some(lifetime.as_secs() as i64),
    };

    let created = async {
        for file in &files {
            state.storage.create(&file.id).await?;
        }
        db::insert_box(&state.db, &new_box).await?;
        Ok::<_, AppError>(())
    }
    .await;
    if let Err(error) = created {
        discard(&state, &files).await;
        return Err(error);
    }

    tracing::info!(code = %new_box.record.code, files = files.len(), "upload started");
    Ok(HttpResponse::Created().json(Started {
        code: new_box.record.code,
        delete_token,
        chunk_size: CHUNK_SIZE,
        upload_expires_at,
        files,
    }))
}

async fn status(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<web::Json<Progress>, AppError> {
    let pending = owned_pending_box(&state, &request, &code).await?;
    let mut files = Vec::new();
    for file in db::box_files(&state.db, &pending.code).await? {
        let received = state.storage.size(&file.id).await?;
        files.push(FileProgress { file, received });
    }
    Ok(web::Json(Progress { files }))
}

async fn append(
    state: web::Data<AppState>,
    request: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ChunkQuery>,
    body: web::Payload,
) -> Result<web::Json<Received>, AppError> {
    let (code, file_id) = path.into_inner();
    let pending = owned_pending_box(&state, &request, &code).await?;
    let file = db::box_files(&state.db, &pending.code)
        .await?
        .into_iter()
        .find(|file| file.id == file_id)
        .ok_or(AppError::NotFound)?;

    let offset = query.offset;
    let remaining = (file.size as u64)
        .checked_sub(offset)
        .ok_or_else(|| AppError::BadRequest("offset is past the end of the file".into()))?;
    let declared = api::header_str(&request, header::CONTENT_LENGTH.as_str())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > remaining) {
        return Err(past_end());
    }

    let received = state
        .storage
        .append(&file.id, offset, body, remaining)
        .await
        .map_err(|error| match error {
            WriteError::TooLarge => past_end(),
            other => AppError::from(other),
        })?;
    Ok(web::Json(Received { received }))
}

async fn complete(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<web::Json<BoxView>, AppError> {
    let pending = owned_pending_box(&state, &request, &code).await?;
    let files = db::box_files(&state.db, &pending.code).await?;
    for file in &files {
        if state.storage.size(&file.id).await? != file.size as u64 {
            return Err(AppError::Conflict(format!(
                "`{}` has not been fully uploaded",
                file.name
            )));
        }
    }

    let now = unix_now();
    let record = BoxRecord {
        code: pending.code,
        created_at: now,
        expires_at: now + pending.pending_lifetime,
    };
    if !db::complete_box(&state.db, &record).await? {
        return Err(AppError::NotFound);
    }

    let total: i64 = files.iter().map(|file| file.size).sum();
    tracing::info!(code = %record.code, files = files.len(), bytes = total, "box created");
    Ok(web::Json(api::box_view(&state, &request, record, files, None)))
}

/// Finds a pending box and checks that the request carries its delete token.
async fn owned_pending_box(
    state: &AppState,
    request: &HttpRequest,
    code: &str,
) -> Result<PendingBox, AppError> {
    if !token::is_share_code(code) {
        return Err(AppError::NotFound);
    }
    let pending = db::find_pending_box(&state.db, code, unix_now())
        .await?
        .ok_or(AppError::NotFound)?;
    let given = api::header_str(request, DELETE_TOKEN_HEADER).ok_or(AppError::Forbidden)?;
    if token::hash(given) == pending.delete_token_hash {
        Ok(pending)
    } else {
        Err(AppError::Forbidden)
    }
}

fn past_end() -> AppError {
    AppError::BadRequest("chunk goes past the declared file size".into())
}
