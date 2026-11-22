use std::io::{self, SeekFrom};
use std::time::Duration;

use axum::body::Body;
use bytes::{Bytes, BytesMut};
use futures_util::stream;
use kob_usenet::OutputReader;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::time::timeout;

const CHUNK: usize = 256 * 1024;

pub struct Pump {
    pub reader: OutputReader,
    pub at: u64,
    pub remaining: u64,
    pub gap: Duration,
    pub label: String,
}

impl Pump {
    pub fn into_body(self) -> Body {
        Body::from_stream(stream::unfold(
            (Some(self), false),
            |(pump, sought)| async move {
                let mut pump = pump.filter(|p| p.remaining > 0)?;
                match pump.next(sought).await {
                    Ok(bytes) => Some((Ok(bytes), (Some(pump), true))),
                    Err(error) => {
                        tracing::warn!(
                            stream = pump.label,
                            at = pump.at,
                            left = pump.remaining,
                            %error,
                            "stream aborted"
                        );
                        Some((Err(error), (None, true)))
                    }
                }
            },
        ))
    }

    async fn next(&mut self, sought: bool) -> io::Result<Bytes> {
        if !sought {
            timeout(self.gap, self.reader.seek(SeekFrom::Start(self.at)))
                .await
                .map_err(|_| self.silent())??;
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
