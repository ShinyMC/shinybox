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
