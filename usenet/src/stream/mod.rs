mod body;
mod range;

use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use kob_usenet::ReleaseKey;
use usenet_proto::StreamClaims;

use crate::error::ApiError;
use crate::state::AppState;
use body::Pump;
use range::{Span, span};

pub async fn stream(
    State(state): State<AppState>,
    method: Method,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let claims = usenet_proto::verify(&state.stream_secret, &token, now()).map_err(|error| {
        tracing::debug!(%error, "stream token refused");
        ApiError::new(StatusCode::FORBIDDEN, "forbidden")
    })?;
    let key: ReleaseKey = claims
        .release_key
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "unknown_release"))?;
    let release = state.engine.release(&key)?;
    let output = release
        .outputs
        .get(claims.output)
        .ok_or(kob_usenet::Error::UnknownOutput {
            key: key.clone(),
            index: claims.output,
        })?;
    let len = output.size;
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let mut response = Response::builder().header(header::ACCEPT_RANGES, "bytes");
    let (start, count) = match span(range, len) {
        Span::Unsatisfiable => {
            return response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Body::empty())
                .map_err(internal);
        }
        Span::Full => {
            response = response.status(StatusCode::OK);
            (0, len)
        }
        Span::Partial { start, end } => {
            response = response
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
            (start, end - start + 1)
        }
    };
    let content_type = mime_guess::from_path(&output.path).first_or_octet_stream();
    response = response
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type.as_ref()).map_err(internal)?,
        )
        .header(header::CONTENT_LENGTH, count);

    if method == Method::HEAD || count == 0 {
        return response.body(Body::empty()).map_err(internal);
    }
    let mut reader = state.engine.open(&key, claims.output, purpose(&claims))?;
    reader.set_end(start + count);
    tracing::debug!(%key, output = claims.output, start, count, purpose = ?claims.purpose, "stream");
    let pump = Pump {
        reader,
        at: start,
        remaining: count,
        gap: state.byte_gap,
        label: format!("{key}/{}", claims.output),
    };
    response.body(pump.into_body()).map_err(internal)
}

const fn purpose(claims: &StreamClaims) -> kob_usenet::Purpose {
    match claims.purpose {
        usenet_proto::Purpose::Play => kob_usenet::Purpose::Play,
        usenet_proto::Purpose::Cache => kob_usenet::Purpose::Cache,
        usenet_proto::Purpose::Warm => kob_usenet::Purpose::Warm,
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!(%error, "could not build the stream response");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}
