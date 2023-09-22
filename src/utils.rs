use actix_web::HttpResponse;

pub fn success(message: &str) -> HttpResponse {
    HttpResponse::Ok().body(format!("Success: {}", &message))
}

pub fn error(message: &str) -> HttpResponse {
    HttpResponse::BadRequest().body(format!("Error: {}", &message))
}

pub fn get_size_suffix(size: &usize) -> String {
    let mut size = size.clone() as f64;
    let mut suffix = String::from("b");

    if size > 1024.0 {
        size /= 1024.0;
        suffix = String::from("kb");
    }

    if size > 1024.0 {
        size /= 1024.0;
        suffix = String::from("mb");
    }

    return format!("{:.2}{}", size, suffix)
}

pub fn get_file_extension(file_name: &str) -> String {
    let mut file_extension: String = String::from("");

    for c in (&file_name).chars().rev() {
        if c == '.' { break; }
        file_extension.insert(0, c);
    }

    return file_extension;
}