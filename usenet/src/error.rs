use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kob_usenet::vendor::queue::import;
use serde_json::json;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: Option<String>,
}

impl ApiError {
    pub const fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            message: None,
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

impl From<kob_usenet::Error> for ApiError {
    fn from(error: kob_usenet::Error) -> Self {
        use kob_usenet::Error as E;
        let (status, code) = match &error {
            E::NotCached => (StatusCode::NOT_FOUND, "not_cached"),
            E::UnknownRelease(_) | E::UnknownOutput { .. } => {
                (StatusCode::NOT_FOUND, "unknown_release")
            }
            E::Nzb(_) => (StatusCode::BAD_REQUEST, "invalid_nzb"),
            E::NestedNzb => (StatusCode::UNPROCESSABLE_ENTITY, "nested_nzb"),
            E::Retention => (StatusCode::UNPROCESSABLE_ENTITY, "retention"),
            E::Import(import::Error::Missing(_)) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "missing_articles")
            }
            E::Import(import::Error::Password(_)) => (StatusCode::UNPROCESSABLE_ENTITY, "password"),
            E::Import(import::Error::Unsupported(_)) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "unsupported")
            }
            E::Import(import::Error::Retry(_)) | E::Server { .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, "providers_unavailable")
            }
            E::Import(_) | E::Stored(_) | E::Reader(_) | E::Io(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        if status.is_server_error() {
            tracing::warn!(%error, code, "request failed");
        }
        match status {
            StatusCode::NOT_FOUND => Self::new(status, code),
            _ => Self::new(status, code).with_message(error.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.message {
            Some(message) => json!({"error": self.code, "message": message}),
            None => json!({"error": self.code}),
        };
        (self.status, Json(body)).into_response()
    }
}
