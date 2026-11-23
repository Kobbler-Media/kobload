use std::io::{self, SeekFrom};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::{StreamExt as _, stream};
use kob_usenet::OutputReader;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::{Instant, timeout, timeout_at};

use crate::damage::{Damage, ReadFailure};

const CHUNK: usize = 256 * 1024;

pub struct First {
    pub bytes: Bytes,
    pub failure: Option<(ReadFailure, io::Error)>,
}

pub struct Pump {
    pub reader: OutputReader,
    pub at: u64,
    pub remaining: u64,
    pub gap: Duration,
    pub label: String,
    pub damage: Arc<Damage>,
    pub sought: bool,
}

impl Pump {
    // A failure before the headers can still change the status, so the first chunk is read
    // up front. Bytes read before the failure are returned with it.
    pub async fn first(&mut self) -> First {
        let deadline = Instant::now() + self.gap;
        let mut first = BytesMut::new();
        while first.len() < CHUNK && self.remaining > 0 {
            match timeout_at(deadline, self.next()).await {
                Ok(Ok(bytes)) => first.extend_from_slice(&bytes),
                Ok(Err(error)) => {
                    let failure = self.failed(&error, "stream refused");
                    return First {
                        bytes: first.freeze(),
                        failure: Some((failure, error)),
                    };
                }
                Err(_) if !first.is_empty() => break,
                Err(_) => {
                    let error = self.silent();
                    let failure = self.failed(&error, "stream refused");
                    return First {
                        bytes: Bytes::new(),
                        failure: Some((failure, error)),
                    };
                }
            }
        }
        First {
            bytes: first.freeze(),
            failure: None,
        }
    }

    // A short body that ended cleanly reads as end of file to a transcoder, so the promised
    // length stands and the connection is aborted after the good bytes. The yield lets hyper
    // flush them: an error in the same poll drops the buffered headers and data unsent.
    pub fn aborted_body(good: Bytes, error: io::Error) -> Body {
        Body::from_stream(
            stream::once(async { Ok(good) }).chain(stream::once(async move {
                tokio::task::yield_now().await;
                Err(error)
            })),
        )
    }

    pub fn into_body(self, first: Bytes) -> Body {
        Body::from_stream(stream::unfold(
            (Some(self), Some(first)),
            |(pump, first)| async move {
                if let Some(first) = first {
                    return Some((Ok(first), (pump, None)));
                }
                let mut pump = pump.filter(|p| p.remaining > 0)?;
                match pump.next().await {
                    Ok(bytes) => Some((Ok(bytes), (Some(pump), None))),
                    Err(error) => {
                        pump.failed(&error, "stream aborted");
                        Some((Err(error), (None, None)))
                    }
                }
            },
        ))
    }

    fn failed(&self, error: &io::Error, what: &'static str) -> ReadFailure {
        let failure = ReadFailure::of(error);
        self.damage.record(failure);
        tracing::warn!(
            stream = self.label,
            at = self.at,
            left = self.remaining,
            code = failure.code(),
            damaged = failure.is_damage(),
            %error,
            "{what}"
        );
        failure
    }

    async fn next(&mut self) -> io::Result<Bytes> {
        if !self.sought {
            timeout(self.gap, self.reader.seek(SeekFrom::Start(self.at)))
                .await
                .map_err(|_| self.silent())??;
            self.sought = true;
        }
        let want = usize::try_from(self.remaining).map_or(CHUNK, |r| r.min(CHUNK));
        let mut buf = BytesMut::zeroed(want);
        let read = timeout(self.gap, self.reader.read(&mut buf))
            .await
            .map_err(|_| self.silent())??;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "output ended early",
            ));
        }
        buf.truncate(read);
        self.at += read as u64;
        self.remaining -= read as u64;
        Ok(buf.freeze())
    }

    fn silent(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no bytes within {:?}", self.gap),
        )
    }
}
