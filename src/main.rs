mod utils;

use std::io::{ Result };
use std::path::Path;

use actix_web::{App, get, HttpRequest, HttpResponse, HttpServer, post, web};
use actix_multipart::Multipart;
use actix_web::http::header::CONTENT_LENGTH;
use futures::{TryStreamExt};
use tokio::fs;
use tokio::io::AsyncWriteExt as _;

#[actix_web::main]
async fn main() -> Result<()> {
    if !Path::new("./uploads").exists() {
        fs::create_dir("./uploads").await?;
    }

    HttpServer::new(|| {
        App::new()
            .service(welcome)
            .service(list_files)
            .service(upload)
            .service(download)
    })
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}

#[get("/")]
async fn welcome() -> HttpResponse {
    HttpResponse::Ok().body(include_str!("./../client/index.html"))
}

struct File {
    id: usize,
    name: String,
    extension: String,
    size: usize,
    size_suffix: String
}

const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;
const MAX_FILE_COUNT: usize = 3;

#[post("/upload")]
async fn upload(mut payload: Multipart, request: HttpRequest) -> HttpResponse {
    // limit the file size
    let content_size: usize = match request.headers().get(CONTENT_LENGTH) {
        Some(hv) => hv.to_str().unwrap_or("0").parse().unwrap(),
        None => 0
    };

    if content_size == 0 {
        return utils::error("The file is empty or missing");
    }
    else if content_size > MAX_FILE_SIZE {
        return utils::error("Maximum file size exceeded (10 MB)");
    }

    let mut files: Vec<File> = Vec::new();

    loop {
        // limit the file count
        if files.len() >= MAX_FILE_COUNT { break; }

        // actual file handling goes here
        if let Ok(Some(mut field)) = payload.try_next().await {
            //if field.name().eq("upload").not() { continue; }

            if field.content_type().is_none() { continue; }

            let file_name = field.content_disposition().get_filename().unwrap().to_string();
            let file_extension = utils::get_file_extension(&file_name);

            let destination = format!("./uploads/{}", &file_name);
            let mut saved_file: fs::File = fs::File::create(&destination).await.unwrap();

            while let Ok(Some(chunk)) = field.try_next().await {
                let _ = saved_file.write_all(&chunk).await.unwrap();
            }

            let file_size = content_size;
            let file_size_suffix = utils::get_size_suffix(&file_size);

            // limit the file count
            if files.len() >= MAX_FILE_COUNT {
                println!("Stopped uploading files because exceeded allowed file count (3)");
                break;
            }

            // limit the file size
            if file_size > MAX_FILE_SIZE {
                println!("Skipped file `{}` because it's exceeded the max size (10 MB)", file_name);
                continue;
            }

            let file = File {
                id: 1,//db::generate_id(), // TODO: get the generated ID from the database
                name: file_name.to_string(),
                extension: file_extension,
                size: file_size,
                size_suffix: file_size_suffix
            };

            // handle folders
            if file.extension == "" { // TODO: learn if you can zip them and tag them to know they're folders and unzip them when downloading
                println!("Skipped file `{}` because it's a folder", &file.name);
                continue;
            }

            // save the file
            let file_path = format!(".uploads/{}", &file_name);

            let _ = web::block(|| fs::File::create(file_path))
                .await
                .unwrap();

            files.push(file);
        } else { break; }
    }

    let file_count = files.len();
    if file_count == 0 { return utils::error("no file"); } // TODO: return JSON response

    let mut file_names = String::new();

    for file in &files {
        let size = file.size.to_string() + String::from(&file.size_suffix).as_str();
        let name = String::from(&file.name) + "(" + &size + "), ";

        file_names += &name;
    }

    let file_names = file_names.trim_end_matches(", ");
    let total_size = &files.iter().map(|f| &f.size).sum();
    let total_size_suffix = utils::get_size_suffix(total_size);

    println!("{} files uploaded ({}) with the total size of {}{}", file_count, file_names, total_size, total_size_suffix);
    return utils::success("success"); // TODO: return JSON response
}

#[post("/files")]
async fn list_files() -> HttpResponse {
    HttpResponse::Ok().body("list files")
}

#[get("/download/{id}")]
async fn download() -> HttpResponse {
    HttpResponse::Ok().body("download")
}