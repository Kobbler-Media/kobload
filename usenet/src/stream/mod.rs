mod body;
mod range;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use kob_usenet::ReleaseKey;
use usenet_proto::StreamClaims;

use crate::error::ApiError;
use crate::state::AppState;
use body::{First, Pump};
use range::{Span, span};

pub async fn stream(
    State(state): State<AppState>,
    method: Method,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let claims = usenet_proto::verify(&state.stream_secret, &token, now()).map_err(|error| {
        match state.refused_tokens.due(Instant::now()) {
            Some(held_back) => tracing::warn!(%error, held_back, "stream token refused"),
            None => tracing::debug!(%error, "stream token refused"),
        }
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

    let (start, count, ranged) = match span(range, len) {
        Span::Unsatisfiable => {
            return Response::builder()
                .header(header::ACCEPT_RANGES, "bytes")
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Body::empty())
                .map_err(internal);
        }
        Span::Full => (0, len, false),
        Span::Partial { start, end } => (start, end - start + 1, true),
    };
    let content_type = mime_guess::from_path(&output.path).first_or_octet_stream();
    let content_type = HeaderValue::from_str(content_type.as_ref()).map_err(internal)?;
    let reply = |count: u64| {
        let builder = Response::builder()
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_TYPE, content_type.clone())
            .header(header::CONTENT_LENGTH, count);
        if ranged {
            builder.status(StatusCode::PARTIAL_CONTENT).header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{}/{len}", start + count - 1),
            )
        } else {
            builder.status(StatusCode::OK)
        }
    };

    if method == Method::HEAD || count == 0 {
        return reply(count).body(Body::empty()).map_err(internal);
    }
    let mut reader = state.engine.open(&key, claims.output, purpose(&claims))?;
    reader.set_end(start + count);
    tracing::debug!(%key, output = claims.output, start, count, purpose = ?claims.purpose, "stream");
    let mut pump = Pump {
        reader,
        at: start,
        remaining: count,
        gap: state.byte_gap,
        label: format!("{key}/{}", claims.output),
        damage: state.damage.clone(),
        sought: false,
    };
    let First { bytes, failure } = pump.first().await;
    match failure {
        None => reply(count).body(pump.into_body(bytes)).map_err(internal),
        // A shorter 206 is only legal for a Range request; the client re-requests from its end.
        Some(_) if ranged && !bytes.is_empty() => reply(bytes.len() as u64)
            .body(Body::from(bytes))
            .map_err(internal),
        Some(failure) => Err(ApiError::new(failure.status(), failure.code())),
    }
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
