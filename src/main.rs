use std::io::Result;

use actix_multipart::Multipart;
use actix_web::{App, get, HttpRequest, HttpResponse, HttpServer, post, web};
use actix_web::http::header::CONTENT_LENGTH;
use futures::TryStreamExt;
use serde::Serialize;
use tokio::fs;
use tokio::io::AsyncWriteExt as _;

mod utils;
mod database;

#[actix_web::main]
async fn main() -> Result<()> {
    utils::create_dir("../../uploads");

    HttpServer::new(|| {
        let api = web::scope("/api/v1")
            .service(list_files)
            .service(upload)
            .service(download);

        App::new()
            .service(page_content)
            .service(page_style)
            .service(page_script)
            .service(api)
            .default_service(web::route().to(|| HttpResponse::NotFound()))
    })
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}

#[get("/")]
async fn page_content() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(include_str!("../frontend/index.html"))
}

#[get("/style.css")]
async fn page_style() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/css; charset=utf-8")
        .body(include_str!("../frontend/style.css"))
}

#[get("/script.js")]
async fn page_script() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/javascript; charset=utf-8")
        .body(include_str!("../frontend/script.js"))
}

// TODO load favicon from file

struct File {
    id: usize,
    name: String,
    token: String,
    size: u64
}

impl File {
    fn set_size(&mut self, size: u64) {
        self.size = size;
    }
}

#[post("/upload")]
async fn upload(mut payload: Multipart, request: HttpRequest) -> HttpResponse {
    let total_size: usize = match request.headers().get(CONTENT_LENGTH) {
        Some(hv) => hv.to_str().unwrap_or("0").parse().unwrap(),
        None => 0
    };

    let mut files: Vec<File> = Vec::new();

    // TODO get these from config.toml
    let max_file_size: usize = 10 * 1024 * 1024;
    let max_file_count: usize = 3;

    // check if the maximum file size is exceeded
    if total_size > max_file_size {
        return json(ErrorResponse {
            status: false,
            error: String::from("Maximum file size exceeded (10MB)")
        });
    }

    // check if there are is any file uploaded
    if total_size == 0 {
        return json(ErrorResponse {
            status: false,
            error: String::from("The file is empty or missing")
        });
    }

    loop {
        // check if the maximum file count is exceeded
        if files.len() > max_file_count {
            return json(ErrorResponse {
                status: false,
                error: String::from("Maximum file count exceeded (3)")
            });
        }

        // actual file handling goes here
        if let Ok(Some(mut field)) = payload.try_next().await {

            // check if this is the right field
            if field.name() != "upload" { continue; }

            // check if this is a file
            if field.content_type().is_none() { continue; }

            let file_name = field.content_disposition().get_filename().unwrap().to_string();

            let destination = format!("./uploads/{}", &file_name);
            let mut saved_file: fs::File = fs::File::create(&destination).await.unwrap();

            // write chunks of the file to the destination
            while let Ok(Some(chunk)) = field.try_next().await {
                let _ = saved_file.write_all(&chunk).await.unwrap();
            }

            let file_path = format!(".uploads/{}", &file_name);

            let file = File {
                id: 1,//db::generate_id(), // TODO: get the generated ID from the database
                name: file_name.to_string(),
                token: utils::generate_token(),
                size: 0
            };

            // create the file
            let _ = web::block(|| fs::File::create(file_path))
                .await
                .unwrap();

            files.push(file);
        }
        else { break; }
    }

    // check if there are any files uploaded at all
    if files.len() == 0 {
        return json(ErrorResponse {
            status: false,
            error: String::from("No files were uploaded")
        });
    }

    let mut file_names = String::new();

    for file in &mut files {
        let size = utils::get_file_size(&file.name).unwrap_or_else(|_| 0);

        file.set_size(size);

        file_names.push_str(&file.name);
        file_names.push_str(", ");
    }

    let file_names = file_names.trim_end_matches(", ");
    let readable_total_size = utils::get_readable_size(&total_size);

    println!("{} files uploaded: ({}) with the total size of {}", files.len(), &file_names, &readable_total_size);

    // i think i am doing these wrong search about "struct"
    return json(UploadResponse {
        status: true,
        token: utils::generate_token()
    });
}

fn json<T: Serialize>(response: T) -> HttpResponse {
    HttpResponse::Ok().json(response)
}

#[derive(Serialize)]
struct UploadResponse {
    status: bool,
    token: String
}

#[derive(Serialize)]
struct ErrorResponse {
    status: bool,
    error: String
}

#[get("/get/{token}")]
async fn get(token: web::Path<String>) -> HttpResponse {
    if utils::is_file_exists("../../uploads") {
        HttpResponse::Ok().body("exists")
    } else {
        HttpResponse::Ok().body("not exists")
    }
}

#[post("/files")]
async fn list_files() -> HttpResponse {
    HttpResponse::Ok().body("list files")
}

#[get("/download/{id}")]
async fn download() -> HttpResponse {
    HttpResponse::Ok().body("download")
}
