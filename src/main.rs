use std::path::PathBuf;
use std::time::Duration;

use actix_web::HttpServer;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use shinybox::config::Config;
use shinybox::{AppState, cleanup};

const DEFAULT_CONFIG_FILE: &str = "shinybox.toml";
const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// A tiny self-hosted file drop: upload files, share a link, let them expire.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Path to the config file [default: ./shinybox.toml, if it exists]
    #[arg(short, long, env = "SHINYBOX_CONFIG")]
    config: Option<PathBuf>,

    /// Address to listen on, e.g. 0.0.0.0:8080
    #[arg(long, env = "SHINYBOX_BIND")]
    bind: Option<String>,

    /// Directory for the database and uploaded files
    #[arg(long, env = "SHINYBOX_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Base URL used in share links, e.g. https://box.example.com
    #[arg(long, env = "SHINYBOX_PUBLIC_URL")]
    public_url: Option<String>,

    /// Require this bearer token for uploads
    #[arg(long, env = "SHINYBOX_UPLOAD_TOKEN", hide_env_values = true)]
    upload_token: Option<String>,
}

impl Cli {
    fn into_config(self) -> Result<Config, Box<dyn std::error::Error>> {
        let default_file = PathBuf::from(DEFAULT_CONFIG_FILE);
        let mut config = match self.config {
            Some(path) => Config::from_file(&path)?,
            None if default_file.exists() => Config::from_file(&default_file)?,
            None => Config::default(),
        };

        if let Some(bind) = self.bind {
            config.bind = bind;
        }
        if let Some(data_dir) = self.data_dir {
            config.data_dir = data_dir;
        }
        if self.public_url.is_some() {
            config.public_url = self.public_url;
        }
        if self.upload_token.is_some() {
            config.upload_token = self.upload_token;
        }
        Ok(config.validated()?)
    }
}

#[actix_web::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let config = Cli::parse().into_config()?;
    let bind = config.bind.clone();
    let state = AppState::open(config).await?;

    cleanup::spawn(state.clone(), CLEANUP_INTERVAL);

    tracing::info!(
        address = %bind,
        data_dir = %state.config.data_dir.display(),
        max_upload_size = %state.config.max_upload_size_label(),
        upload_token = state.config.upload_token.is_some(),
        "shinybox is listening"
    );

    HttpServer::new(move || shinybox::app(state.clone()))
        .bind(&bind)?
        .run()
        .await?;
    Ok(())
}
