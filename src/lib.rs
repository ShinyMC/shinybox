//! shinybox: a tiny self-hosted file drop.
//!
//! Uploads are grouped into *boxes*. Each box gets an unguessable share code,
//! lives until its expiry time and can be deleted early with a secret token.

pub mod cleanup;
pub mod config;
pub mod db;
pub mod error;
pub mod routes;
pub mod storage;
pub mod token;
pub mod upload;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use actix_web::body::MessageBody;
use actix_web::dev::{ServiceFactory, ServiceRequest, ServiceResponse};
use actix_web::middleware::{DefaultHeaders, Logger};
use actix_web::{App, web};
use sqlx::SqlitePool;

use crate::config::Config;
use crate::storage::Storage;

/// Shared state handed to every request handler.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: SqlitePool,
    pub storage: Storage,
}

impl AppState {
    /// Prepares the data directory, opens the database and applies migrations.
    pub async fn open(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let storage = Storage::open(config.data_dir.join("files")).await?;
        let db = db::open(&config.data_dir.join("shinybox.db")).await?;
        Ok(Self {
            config: Arc::new(config),
            db,
            storage,
        })
    }
}

/// Builds the application with all routes and middleware.
pub fn app(
    state: AppState,
) -> App<
    impl ServiceFactory<
        ServiceRequest,
        Config = (),
        Response = ServiceResponse<impl MessageBody>,
        Error = actix_web::Error,
        InitError = (),
    >,
> {
    App::new()
        .app_data(web::Data::new(state))
        .wrap(security_headers())
        .wrap(Logger::new(r#"%r %s %b %Dms"#))
        .configure(routes::configure)
}

fn security_headers() -> DefaultHeaders {
    DefaultHeaders::new()
        .add(("X-Content-Type-Options", "nosniff"))
        .add(("Referrer-Policy", "no-referrer"))
        .add((
            "Content-Security-Policy",
            "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ))
}

/// Current time in Unix seconds.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}
