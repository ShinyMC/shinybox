//! The web interface: one page, its stylesheet and script, all compiled into the binary.

use actix_web::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use actix_web::{HttpResponse, web};

const INDEX: &str = include_str!("../../web/index.html");
const STYLE: &str = include_str!("../../web/app.css");
const SCRIPT: &str = include_str!("../../web/app.js");
const ICON: &str = include_str!("../../web/favicon.svg");

pub fn configure(cfg: &mut web::ServiceConfig) {
    for page in ["/", "/mine", "/b/{code}"] {
        cfg.route(
            page,
            web::get().to(|| async { asset("text/html; charset=utf-8", INDEX) }),
        );
    }
    cfg.route(
        "/app.css",
        web::get().to(|| async { asset("text/css; charset=utf-8", STYLE) }),
    )
    .route(
        "/app.js",
        web::get().to(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }),
    )
    .route(
        "/favicon.svg",
        web::get().to(|| async { asset("image/svg+xml", ICON) }),
    );
}

fn asset(content_type: &'static str, body: &'static str) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, content_type))
        .insert_header((CACHE_CONTROL, "no-cache"))
        .body(body)
}
