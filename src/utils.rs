use std::fs;
use std::path::Path;

use filesize::PathExt;
use human_bytes::human_bytes;
use rand::Rng;

pub fn is_file_exists(path: &str) -> bool {
    fs::metadata(&path).is_ok()
}

pub fn create_dir(path: &str) {
    if !is_file_exists(&path) {
        fs::create_dir(&path).expect("Failed to create directory");
    }
}

pub fn get_file_size(path: &str) -> Result<u64, &'static str> {
    if !is_file_exists(&path) {
        return Err("File does not exist");
    }

    let path = Path::new(&path);
    let metadata = path.symlink_metadata().unwrap();

    return Ok(path.size_on_disk_fast(&metadata).unwrap());
}

pub fn get_readable_size(size: &u64) -> String {
    human_bytes(size.clone() as f64)
}

pub fn times(n: usize) -> impl Iterator {
    std::iter::repeat(()).take(n)
}

pub fn generate_token() -> String {
    let mut token = String::new();

    for _ in times(8) {
        let random = rand::thread_rng().gen_range(0..10);
        token.push_str(&random.to_string());
    }
    return token;
}