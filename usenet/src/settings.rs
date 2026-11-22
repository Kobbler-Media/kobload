use std::path::PathBuf;

use usenet_proto::{SecretError, check_secrets};

use crate::state::DEFAULT_DOWNLOAD_DIR;

const DEFAULT_PORT: u16 = 7090;
const DEFAULT_DATA_DIR: &str = "/data";
const DEFAULT_MIN_CONNECTIONS: usize = 4;
const DEFAULT_FREE_SPACE_FLOOR_GB: u64 = 1;
const DEFAULT_ACTIVE_JOBS: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is not valid: {value}")]
    Invalid { name: &'static str, value: String },
    #[error(transparent)]
    Weak(#[from] SecretError),
}

pub struct Settings {
    pub port: u16,
    pub data_dir: PathBuf,
    pub kobload_url: String,
    pub control_key: String,
    pub stream_secret: String,
    pub metrics_key: Option<String>,
    pub min_connections: usize,
    pub download_dir: PathBuf,
    pub free_space_floor: u64,
    pub active_jobs: usize,
}

impl Settings {
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, SettingsError> {
        let get = |name| lookup(name).filter(|v| !v.trim().is_empty());
        let required = |name| get(name).ok_or(SettingsError::Missing(name));
        let settings = Self {
            port: parsed(get("USENET_PORT"), "USENET_PORT")?.unwrap_or(DEFAULT_PORT),
            data_dir: get("USENET_DATA_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from),
            kobload_url: required("KOBLOAD_URL")?,
            control_key: required("USENET_CONTROL_KEY")?,
            stream_secret: required("USENET_STREAM_SECRET")?,
            metrics_key: get("USENET_METRICS_KEY"),
            min_connections: parsed(get("USENET_MIN_CONNECTIONS"), "USENET_MIN_CONNECTIONS")?
                .unwrap_or(DEFAULT_MIN_CONNECTIONS),
            download_dir: download_dir(get("USENET_DOWNLOAD_DIR"))?,
            free_space_floor: parsed::<u64>(
                get("USENET_FREE_SPACE_FLOOR_GB"),
                "USENET_FREE_SPACE_FLOOR_GB",
            )?
            .unwrap_or(DEFAULT_FREE_SPACE_FLOOR_GB)
            .saturating_mul(1 << 30),
            active_jobs: active_jobs(get("USENET_ACTIVE_JOBS"))?,
        };
        let mut secrets = vec![
            ("USENET_CONTROL_KEY", settings.control_key.as_str()),
            ("USENET_STREAM_SECRET", settings.stream_secret.as_str()),
        ];
        if let Some(metrics_key) = &settings.metrics_key {
            secrets.push(("USENET_METRICS_KEY", metrics_key));
        }
        check_secrets(&secrets)?;
        Ok(settings)
    }
}

// kobload-api reads a finished job's folder by the path the engine reports, so the engine
// and the api must mount downloads at the same absolute path.
fn download_dir(value: Option<String>) -> Result<PathBuf, SettingsError> {
    let dir = value.map_or_else(|| PathBuf::from(DEFAULT_DOWNLOAD_DIR), PathBuf::from);
    if dir.is_absolute() {
        Ok(dir)
    } else {
        Err(SettingsError::Invalid {
            name: "USENET_DOWNLOAD_DIR",
            value: dir.display().to_string(),
        })
    }
}

fn active_jobs(value: Option<String>) -> Result<usize, SettingsError> {
    match parsed::<usize>(value.clone(), "USENET_ACTIVE_JOBS")? {
        None => Ok(DEFAULT_ACTIVE_JOBS),
        Some(0) => Err(SettingsError::Invalid {
            name: "USENET_ACTIVE_JOBS",
            value: value.unwrap_or_default(),
        }),
        Some(n) => Ok(n),
    }
}

fn parsed<T: std::str::FromStr>(
    value: Option<String>,
    name: &'static str,
) -> Result<Option<T>, SettingsError> {
    value
        .map(|value| {
            value
                .trim()
                .parse()
                .map_err(|_| SettingsError::Invalid { name, value })
        })
        .transpose()
}
