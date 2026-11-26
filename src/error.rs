use actix_web::http::StatusCode;
use actix_web::{HttpResponse, ResponseError};
use serde::Serialize;

/// Errors returned by HTTP handlers. Each variant maps to one status code and is
/// rendered as `{"error": "<message>"}`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("a valid upload token is required")]
    Unauthorized,
    #[error("the delete token does not match")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("upload exceeds the {0} limit")]
    TooLarge(String),
    #[error("internal server error")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl AppError {
    pub fn internal(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Internal(error.into())
    }
}

impl From<sqlx::Error> for AppError {
    fn from(error: sqlx::Error) -> Self {
        Self::internal(error)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        Self::internal(error)
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        if let Self::Internal(source) = self {
            tracing::error!(error = %source, "request failed");
        }
        HttpResponse::build(self.status_code()).json(ErrorBody {
            error: &self.to_string(),
        })
    }
}
