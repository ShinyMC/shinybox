use actix_web::body::MessageBody;
use actix_web::dev::{Service, ServiceResponse};
use actix_web::http::StatusCode;
use actix_web::http::header::{AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::test::{self, TestRequest};
use bytesize::ByteSize;
use serde_json::Value;
use shinybox::config::Config;
use shinybox::{AppState, cleanup};
use tempfile::TempDir;

const BOUNDARY: &str = "shinybox-test-boundary";

struct TestServer {
    state: AppState,
    _data_dir: TempDir,
}

impl TestServer {
    async fn start(configure: impl FnOnce(&mut Config)) -> Self {
        let data_dir = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: data_dir.path().to_owned(),
            public_url: Some("https://box.test".into()),
            ..Config::default()
        };
        configure(&mut config);
        let state = AppState::open(config.validated().unwrap()).await.unwrap();
        Self {
            state,
            _data_dir: data_dir,
        }
    }

    async fn call(&self, request: TestRequest) -> ServiceResponse<impl MessageBody> {
        let app = test::init_service(shinybox::app(self.state.clone())).await;
        app.call(request.to_request()).await.unwrap()
    }

    fn stored_files(&self) -> usize {
        std::fs::read_dir(self.state.config.data_dir.join("files"))
            .unwrap()
            .count()
    }
}

/// Builds a `multipart/form-data` body from `(field, filename, contents)` parts.
fn multipart(parts: &[(&str, Option<&str>, &[u8])]) -> TestRequest {
    let mut body = Vec::new();
    for (field, filename, contents) in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let disposition = match filename {
            Some(name) => format!("form-data; name=\"{field}\"; filename=\"{name}\""),
            None => format!("form-data; name=\"{field}\""),
        };
        body.extend_from_slice(format!("Content-Disposition: {disposition}\r\n\r\n").as_bytes());
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());

    TestRequest::post()
        .uri("/api/boxes")
        .insert_header((CONTENT_TYPE, format!("multipart/form-data; boundary={BOUNDARY}")))
        .set_payload(body)
}

async fn json(response: ServiceResponse<impl MessageBody>) -> Value {
    let bytes = test::read_body(response).await;
    serde_json::from_slice(&bytes).unwrap()
}

async fn upload(server: &TestServer, files: &[(&str, &[u8])]) -> Value {
    let parts: Vec<_> = files
        .iter()
        .map(|(name, data)| ("file", Some(*name), *data))
        .collect();
    let response = server.call(multipart(&parts)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    json(response).await
}

#[actix_web::test]
async fn upload_view_download_and_delete() {
    let server = TestServer::start(|_| {}).await;

    let created = upload(&server, &[("hello.txt", b"hello"), ("data.bin", b"\x00\x01\x02")]).await;
    let code = created["code"].as_str().unwrap();
    let token = created["delete_token"].as_str().unwrap();
    assert_eq!(created["url"], format!("https://box.test/b/{code}"));
    assert_eq!(created["files"][0]["name"], "hello.txt");
    assert_eq!(created["files"][0]["size"], 5);
    assert_eq!(created["files"][0]["content_type"], "text/plain");
    assert_eq!(created["files"][1]["name"], "data.bin");
    assert_eq!(server.stored_files(), 2);

    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let shown = json(response).await;
    assert_eq!(shown["files"], created["files"]);
    assert!(
        shown.get("delete_token").is_none(),
        "the delete token must not leak"
    );

    let file_id = created["files"][0]["id"].as_str().unwrap();
    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}/files/{file_id}")))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let disposition = response
        .headers()
        .get(CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(disposition, "attachment; filename=\"hello.txt\"");
    assert_eq!(
        response.headers().get("content-security-policy").unwrap(),
        "sandbox"
    );
    assert_eq!(test::read_body(response).await, "hello");

    let response = server
        .call(
            TestRequest::delete()
                .uri(&format!("/api/boxes/{code}"))
                .insert_header(("X-Delete-Token", token)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(server.stored_files(), 0);

    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}")))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn file_names_cannot_escape_storage() {
    let server = TestServer::start(|_| {}).await;

    let created = upload(&server, &[("../../etc/passwd", b"nope")]).await;

    assert_eq!(created["files"][0]["name"], "passwd");
    assert_eq!(server.stored_files(), 1);
    assert!(!server.state.config.data_dir.join("etc").exists());
}

#[actix_web::test]
async fn rejects_uploads_over_the_size_limit() {
    let server = TestServer::start(|config| config.max_upload_size = ByteSize::b(8)).await;

    let response = server
        .call(multipart(&[
            ("file", Some("a.txt"), b"12345"),
            ("file", Some("b.txt"), b"67890"),
        ]))
        .await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json(response).await["error"], "upload exceeds the 8 B limit");
    assert_eq!(server.stored_files(), 0, "partial uploads must be cleaned up");
}

#[actix_web::test]
async fn rejects_too_many_files() {
    let server = TestServer::start(|config| config.max_files = 1).await;

    let response = server
        .call(multipart(&[("file", Some("a"), b"a"), ("file", Some("b"), b"b")]))
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await["error"],
        "too many files: a box holds at most 1"
    );
    assert_eq!(server.stored_files(), 0);
}

#[actix_web::test]
async fn rejects_requests_without_files() {
    let server = TestServer::start(|_| {}).await;

    let response = server.call(multipart(&[("expires", None, b"1h")])).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(response).await["error"], "no files were uploaded");
}

#[actix_web::test]
async fn honours_chosen_expiry_within_bounds() {
    let server = TestServer::start(|_| {}).await;

    let response = server
        .call(multipart(&[("expires", None, b"1h"), ("file", Some("a"), b"a")]))
        .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = json(response).await;
    let lifetime = created["expires_at"].as_i64().unwrap() - created["created_at"].as_i64().unwrap();
    assert_eq!(lifetime, 3600);

    let response = server
        .call(multipart(&[("expires", None, b"30d"), ("file", Some("a"), b"a")]))
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(server.stored_files(), 1);
}

#[actix_web::test]
async fn delete_requires_the_right_token() {
    let server = TestServer::start(|_| {}).await;
    let code = upload(&server, &[("a", b"a")]).await["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let uri = format!("/api/boxes/{code}");

    let missing = server.call(TestRequest::delete().uri(&uri)).await;
    let wrong = server
        .call(
            TestRequest::delete()
                .uri(&uri)
                .insert_header(("X-Delete-Token", "guess")),
        )
        .await;

    assert_eq!(missing.status(), StatusCode::FORBIDDEN);
    assert_eq!(wrong.status(), StatusCode::FORBIDDEN);
    assert_eq!(server.stored_files(), 1);
}

#[actix_web::test]
async fn upload_token_is_enforced_when_configured() {
    let server = TestServer::start(|config| config.upload_token = Some("s3cret".into())).await;
    let parts: &[(&str, Option<&str>, &[u8])] = &[("file", Some("a"), b"a")];

    let anonymous = server.call(multipart(parts)).await;
    let wrong = server
        .call(multipart(parts).insert_header((AUTHORIZATION, "Bearer nope")))
        .await;
    let allowed = server
        .call(multipart(parts).insert_header((AUTHORIZATION, "Bearer s3cret")))
        .await;

    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(allowed.status(), StatusCode::CREATED);
}

#[actix_web::test]
async fn expired_boxes_disappear_and_are_swept() {
    let server = TestServer::start(|_| {}).await;
    let created = upload(&server, &[("a", b"a")]).await;
    let code = created["code"].as_str().unwrap();
    let file_id = created["files"][0]["id"].as_str().unwrap();

    sqlx::query("UPDATE boxes SET expires_at = 0")
        .execute(&server.state.db)
        .await
        .unwrap();

    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}")))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}/files/{file_id}")))
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    assert_eq!(cleanup::sweep(&server.state).await.unwrap(), 1);
    assert_eq!(server.stored_files(), 0);
}

#[actix_web::test]
async fn reports_limits_for_the_web_interface() {
    let server = TestServer::start(|config| config.upload_token = Some("x".into())).await;

    let info = json(server.call(TestRequest::get().uri("/api/info")).await).await;

    assert_eq!(info["max_upload_size"], 10_000_000);
    assert_eq!(info["max_files"], 3);
    assert_eq!(info["default_expiry"], 86_400);
    assert_eq!(info["upload_token_required"], true);
}

#[actix_web::test]
async fn serves_pages_with_security_headers() {
    let server = TestServer::start(|_| {}).await;

    for path in ["/", "/mine", "/b/k7Qm2xPa9L"] {
        let response = server.call(TestRequest::get().uri(path)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert!(response.headers().contains_key("content-security-policy"));
    }

    let missing = server.call(TestRequest::get().uri("/nope")).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(json(missing).await["error"], "not found");
}

// Resumable uploads

async fn start_upload(server: &TestServer, declaration: Value) -> ServiceResponse<impl MessageBody> {
    server
        .call(TestRequest::post().uri("/api/uploads").set_json(declaration))
        .await
}

fn chunk(started: &Value, file: usize, offset: u64, data: &'static [u8]) -> TestRequest {
    let code = started["code"].as_str().unwrap();
    let id = started["files"][file]["id"].as_str().unwrap();
    TestRequest::put()
        .uri(&format!("/api/uploads/{code}/files/{id}?offset={offset}"))
        .insert_header(("X-Delete-Token", started["delete_token"].as_str().unwrap()))
        .set_payload(data)
}

fn owned(request: TestRequest, started: &Value) -> TestRequest {
    request.insert_header(("X-Delete-Token", started["delete_token"].as_str().unwrap()))
}

#[actix_web::test]
async fn resumable_upload_in_chunks() {
    let server = TestServer::start(|_| {}).await;
    let response = start_upload(
        &server,
        serde_json::json!({
            "expires": "1h",
            "files": [{ "name": "movie.mp4", "size": 11 }, { "name": "../notes.txt", "size": 2 }]
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let started = json(response).await;
    let code = started["code"].as_str().unwrap();
    assert_eq!(started["files"][1]["name"], "notes.txt");
    assert_eq!(started["files"][0]["content_type"], "video/mp4");
    assert!(started["chunk_size"].as_u64().unwrap() > 0);

    let response = server.call(chunk(&started, 0, 0, b"hello ")).await;
    assert_eq!(json(response).await["received"], 6);

    let hidden = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}")))
        .await;
    assert_eq!(
        hidden.status(),
        StatusCode::NOT_FOUND,
        "pending boxes stay hidden"
    );

    let progress = json(
        server
            .call(owned(
                TestRequest::get().uri(&format!("/api/uploads/{code}")),
                &started,
            ))
            .await,
    )
    .await;
    assert_eq!(progress["files"][0]["received"], 6);
    assert_eq!(progress["files"][1]["received"], 0);

    for (file, offset, data) in [(0, 6, &b"world"[..]), (1, 0, b"ok")] {
        let response = server.call(chunk(&started, file, offset, data)).await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    let response = server
        .call(owned(
            TestRequest::post().uri(&format!("/api/uploads/{code}/complete")),
            &started,
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let completed = json(response).await;
    assert_eq!(completed["url"], format!("https://box.test/b/{code}"));
    let lifetime = completed["expires_at"].as_i64().unwrap() - completed["created_at"].as_i64().unwrap();
    assert_eq!(lifetime, 3600);

    let file_id = started["files"][0]["id"].as_str().unwrap();
    let response = server
        .call(TestRequest::get().uri(&format!("/api/boxes/{code}/files/{file_id}")))
        .await;
    assert_eq!(test::read_body(response).await, "hello world");

    let again = server.call(chunk(&started, 1, 2, b"")).await;
    assert_eq!(
        again.status(),
        StatusCode::NOT_FOUND,
        "a complete box takes no more chunks"
    );
}

#[actix_web::test]
async fn resumable_chunks_must_line_up() {
    let server = TestServer::start(|_| {}).await;
    let started = json(
        start_upload(
            &server,
            serde_json::json!({ "files": [{ "name": "a", "size": 4 }] }),
        )
        .await,
    )
    .await;
    let code = started["code"].as_str().unwrap();

    let skipped = server.call(chunk(&started, 0, 2, b"cd")).await;
    assert_eq!(skipped.status(), StatusCode::CONFLICT);

    let too_long = server.call(chunk(&started, 0, 0, b"abcde")).await;
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(too_long).await["error"],
        "chunk goes past the declared file size"
    );

    let stranger = server
        .call(
            TestRequest::put()
                .uri(&format!(
                    "/api/uploads/{code}/files/{}?offset=0",
                    started["files"][0]["id"].as_str().unwrap()
                ))
                .insert_header(("X-Delete-Token", "guess"))
                .set_payload("ab"),
        )
        .await;
    assert_eq!(stranger.status(), StatusCode::FORBIDDEN);

    server.call(chunk(&started, 0, 0, b"ab")).await;
    let early = server
        .call(owned(
            TestRequest::post().uri(&format!("/api/uploads/{code}/complete")),
            &started,
        ))
        .await;
    assert_eq!(early.status(), StatusCode::CONFLICT);
    assert_eq!(json(early).await["error"], "`a` has not been fully uploaded");
}

#[actix_web::test]
async fn resumable_declarations_respect_limits() {
    let server = TestServer::start(|config| {
        config.max_files = 2;
        config.max_upload_size = ByteSize::b(10);
        config.upload_token = Some("s3cret".into());
    })
    .await;
    let declare = |declaration: Value| {
        TestRequest::post()
            .uri("/api/uploads")
            .insert_header((AUTHORIZATION, "Bearer s3cret"))
            .set_json(declaration)
    };

    let anonymous = start_upload(
        &server,
        serde_json::json!({ "files": [{ "name": "a", "size": 1 }] }),
    )
    .await;
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);

    let too_many = server
        .call(declare(serde_json::json!({
            "files": [{ "name": "a", "size": 1 }, { "name": "b", "size": 1 }, { "name": "c", "size": 1 }]
        })))
        .await;
    assert_eq!(too_many.status(), StatusCode::BAD_REQUEST);

    let too_large = server
        .call(declare(
            serde_json::json!({ "files": [{ "name": "a", "size": 11 }] }),
        ))
        .await;
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let overflow = server
        .call(declare(serde_json::json!({
            "files": [{ "name": "a", "size": u64::MAX }, { "name": "b", "size": u64::MAX }]
        })))
        .await;
    assert_eq!(overflow.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let malformed = server.call(declare(serde_json::json!({ "files": "a" }))).await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert!(json(malformed).await["error"].is_string());

    assert_eq!(server.stored_files(), 0);
}

#[actix_web::test]
async fn unfinished_uploads_can_be_cancelled_or_expire() {
    let server = TestServer::start(|_| {}).await;
    let declaration = serde_json::json!({ "files": [{ "name": "a", "size": 4 }] });

    let cancelled = json(start_upload(&server, declaration.clone()).await).await;
    let response = server
        .call(owned(
            TestRequest::delete().uri(&format!("/api/boxes/{}", cancelled["code"].as_str().unwrap())),
            &cancelled,
        ))
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(server.stored_files(), 0);

    let abandoned = json(start_upload(&server, declaration).await).await;
    server.call(chunk(&abandoned, 0, 0, b"ab")).await;
    sqlx::query("UPDATE boxes SET expires_at = 0")
        .execute(&server.state.db)
        .await
        .unwrap();
    assert_eq!(cleanup::sweep(&server.state).await.unwrap(), 1);
    assert_eq!(server.stored_files(), 0);
}
