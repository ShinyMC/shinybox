mod api;
mod web;

use actix_web::{HttpResponse, web as actix};

use crate::error::AppError;

pub fn configure(cfg: &mut actix::ServiceConfig) {
    cfg.service(actix::scope("/api").configure(api::configure))
        .route(
            "/health",
            actix::get().to(|| async { HttpResponse::Ok().body("ok") }),
        )
        .configure(web::configure)
        .default_service(actix::to(|| async { Err::<HttpResponse, _>(AppError::NotFound) }));
}
