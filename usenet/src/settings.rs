use std::path::PathBuf;

const DEFAULT_PORT: u16 = 7090;
const DEFAULT_DATA_DIR: &str = "/data";

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error("{name} is not valid: {value}")]
    Invalid { name: &'static str, value: String },
}

pub struct Settings {
    pub port: u16,
    pub data_dir: PathBuf,
    pub kobload_url: String,
    pub control_key: String,
    pub stream_secret: String,
}

impl Settings {
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, SettingsError> {
        let get = |name| lookup(name).filter(|v| !v.trim().is_empty());
        let required = |name| get(name).ok_or(SettingsError::Missing(name));
        let port = match get("USENET_PORT") {
            None => DEFAULT_PORT,
            Some(value) => value.parse().map_err(|_| SettingsError::Invalid {
                name: "USENET_PORT",
                value,
            })?,
        };
        Ok(Self {
            port,
            data_dir: get("USENET_DATA_DIR")
                .map_or_else(|| PathBuf::from(DEFAULT_DATA_DIR), PathBuf::from),
            kobload_url: required("KOBLOAD_URL")?,
            control_key: required("USENET_CONTROL_KEY")?,
            stream_secret: required("USENET_STREAM_SECRET")?,
        })
    }
}
