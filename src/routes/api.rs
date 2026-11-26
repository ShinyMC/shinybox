//! JSON API under `/api`.

use actix_files::NamedFile;
use actix_multipart::Multipart;
use actix_web::http::header::{self, ContentDisposition};
use actix_web::{HttpRequest, HttpResponse, web};
use serde::Serialize;

use crate::db::{self, BoxRecord, FileRecord, NewBox};
use crate::error::AppError;
use crate::upload::{self, Upload};
use crate::{AppState, token, unix_now};

/// Room for multipart boundaries and headers on top of the file contents.
const MULTIPART_OVERHEAD: u64 = 64 * 1024;
pub(super) const DELETE_TOKEN_HEADER: &str = "X-Delete-Token";

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(web::scope("/uploads").configure(super::uploads::configure))
        .route("/info", web::get().to(info))
        .route("/boxes", web::post().to(create_box))
        .route("/boxes/{code}", web::get().to(get_box))
        .route("/boxes/{code}", web::delete().to(delete_box))
        .route("/boxes/{code}/files/{file_id}", web::get().to(download));
}

/// Limits the web interface shows before an upload starts.
#[derive(Serialize)]
struct Info {
    max_upload_size: u64,
    max_files: usize,
    default_expiry: u64,
    max_expiry: u64,
    upload_token_required: bool,
}

#[derive(Serialize)]
pub(super) struct BoxView {
    code: String,
    url: String,
    created_at: i64,
    expires_at: i64,
    files: Vec<FileRecord>,
    /// Only present in the response to the upload that created the box.
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_token: Option<String>,
}

async fn info(state: web::Data<AppState>) -> web::Json<Info> {
    let config = &state.config;
    web::Json(Info {
        max_upload_size: config.max_upload_size.as_u64(),
        max_files: config.max_files,
        default_expiry: config.default_expiry.as_secs(),
        max_expiry: config.max_expiry.as_secs(),
        upload_token_required: config.upload_token.is_some(),
    })
}

async fn create_box(
    state: web::Data<AppState>,
    request: HttpRequest,
    multipart: Multipart,
) -> Result<HttpResponse, AppError> {
    authorize_upload(&state, &request)?;
    reject_oversized(&state, &request)?;

    let Upload { files, expiry } = upload::receive(&state, multipart).await?;
    if files.is_empty() {
        return Err(AppError::BadRequest("no files were uploaded".into()));
    }

    let now = unix_now();
    let lifetime = expiry.unwrap_or(state.config.default_expiry);
    let record = BoxRecord {
        code: token::share_code(),
        created_at: now,
        expires_at: now + lifetime.as_secs() as i64,
    };
    let delete_token = token::secret();
    let new_box = NewBox {
        record,
        delete_token_hash: token::hash(&delete_token),
        files: &files,
        pending_lifetime: None,
    };

    if let Err(error) = db::insert_box(&state.db, &new_box).await {
        upload::discard(&state, &files).await;
        return Err(error.into());
    }

    let total: i64 = files.iter().map(|file| file.size).sum();
    tracing::info!(code = %new_box.record.code, files = files.len(), bytes = total, "box created");

    let view = box_view(&state, &request, new_box.record, files, Some(delete_token));
    Ok(HttpResponse::Created().json(view))
}

async fn get_box(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<web::Json<BoxView>, AppError> {
    let record = find_box(&state, &code).await?;
    let files = db::box_files(&state.db, &record.code).await?;
    Ok(web::Json(box_view(&state, &request, record, files, None)))
}

async fn download(
    state: web::Data<AppState>,
    request: HttpRequest,
    path: web::Path<(String, String)>,
) -> Result<HttpResponse, AppError> {
    let (code, file_id) = path.into_inner();
    if !token::is_share_code(&code) {
        return Err(AppError::NotFound);
    }
    let file = db::find_file(&state.db, &code, &file_id, unix_now())
        .await?
        .ok_or(AppError::NotFound)?;

    let disk_path = state.storage.path(&file.id);
    let named = web::block(move || NamedFile::open(disk_path))
        .await
        .map_err(AppError::internal)??;
    let content_type = file
        .content_type
        .parse()
        .unwrap_or(mime_guess::mime::APPLICATION_OCTET_STREAM);

    let mut response = named
        .set_content_type(content_type)
        .set_content_disposition(ContentDisposition::attachment(file.name))
        .into_response(&request);
    // Never let a browser execute an uploaded file in this origin.
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static("sandbox"),
    );
    Ok(response)
}

async fn delete_box(
    state: web::Data<AppState>,
    request: HttpRequest,
    code: web::Path<String>,
) -> Result<HttpResponse, AppError> {
    if !token::is_share_code(&code) {
        return Err(AppError::NotFound);
    }
    let expected = db::delete_token_hash(&state.db, &code, unix_now())
        .await?
        .ok_or(AppError::NotFound)?;
    let given = header_str(&request, DELETE_TOKEN_HEADER).ok_or(AppError::Forbidden)?;
    if token::hash(given) != expected {
        return Err(AppError::Forbidden);
    }

    let file_ids = db::delete_box(&state.db, &code).await?;
    for id in &file_ids {
        state.storage.remove(id).await;
    }
    tracing::info!(code = %code.as_str(), "box deleted");
    Ok(HttpResponse::NoContent().finish())
}

async fn find_box(state: &AppState, code: &str) -> Result<BoxRecord, AppError> {
    if !token::is_share_code(code) {
        return Err(AppError::NotFound);
    }
    db::find_box(&state.db, code, unix_now())
        .await?
        .ok_or(AppError::NotFound)
}

pub(super) fn authorize_upload(state: &AppState, request: &HttpRequest) -> Result<(), AppError> {
    let Some(expected) = &state.config.upload_token else {
        return Ok(());
    };
    let given = header_str(request, header::AUTHORIZATION.as_str())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    if token::secrets_match(given, expected) {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

/// Fails fast when the declared body size already exceeds the limit, before
/// anything is written to disk. The streaming limit still applies afterwards.
fn reject_oversized(state: &AppState, request: &HttpRequest) -> Result<(), AppError> {
    let declared =
        header_str(request, header::CONTENT_LENGTH.as_str()).and_then(|value| value.parse::<u64>().ok());
    let limit = state.config.max_upload_size.as_u64();
    match declared {
        Some(length) if length > limit + MULTIPART_OVERHEAD => {
            Err(AppError::TooLarge(state.config.max_upload_size_label()))
        }
        _ => Ok(()),
    }
}

pub(super) fn box_view(
    state: &AppState,
    request: &HttpRequest,
    record: BoxRecord,
    files: Vec<FileRecord>,
    delete_token: Option<String>,
) -> BoxView {
    BoxView {
        url: share_url(state, request, &record.code),
        code: record.code,
        created_at: record.created_at,
        expires_at: record.expires_at,
        files,
        delete_token,
    }
}

fn share_url(state: &AppState, request: &HttpRequest, code: &str) -> String {
    match &state.config.public_url {
        Some(base) => format!("{base}/b/{code}"),
        None => {
            let connection = request.connection_info();
            format!("{}://{}/b/{code}", connection.scheme(), connection.host())
        }
    }
}

pub(super) fn header_str<'a>(request: &'a HttpRequest, name: &str) -> Option<&'a str> {
    request.headers().get(name)?.to_str().ok()
}
