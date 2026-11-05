use crate::protocol::{Event, FEATURE_HISTORY, Status, TranslateRequest};
use futures::{Stream, StreamExt, stream};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::time::Instant;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_START: Duration = Duration::from_millis(50);
const BACKOFF_CAP: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot reach daemon at {}: {source}", socket_path.display())]
    DaemonUnreachable {
        socket_path: PathBuf,
        source: std::io::Error,
    },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("daemon does not support translation history; run \"plamo-translate daemon restart\"")]
    HistoryUnsupported,
}

#[derive(Debug, Clone)]
pub struct Client {
    socket_path: PathBuf,
    connect_timeout: Duration,
}

/// Dropping the stream closes the connection, which the daemon treats as cancellation.
pub struct EventStream {
    inner: Pin<Box<dyn Stream<Item = Result<Event, Error>> + Send>>,
}

impl Stream for EventStream {
    type Item = Result<Event, Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(cx)
    }
}

impl Client {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
        }
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub async fn translate(&self, req: &TranslateRequest) -> Result<EventStream, Error> {
        if !req.history.is_empty() {
            self.require_history().await?;
        }
        let line = serde_json::to_string(req).map_err(|e| Error::Protocol(e.to_string()))?;
        self.request(line).await
    }

    /// Asks the daemon on every call: a cached answer would go stale if the daemon is rolled back.
    pub async fn require_history(&self) -> Result<(), Error> {
        if self.status().await?.supports(FEATURE_HISTORY) {
            Ok(())
        } else {
            Err(Error::HistoryUnsupported)
        }
    }

    pub async fn load(&self) -> Result<EventStream, Error> {
        self.request(r#"{"v":1,"op":"load"}"#.to_string()).await
    }

    pub async fn status(&self) -> Result<Status, Error> {
        let mut events = self.request(r#"{"v":1,"op":"status"}"#.to_string()).await?;
        match events.next().await {
            Some(Ok(Event::Status(s))) => Ok(s),
            Some(Ok(Event::Error { code, message })) => {
                Err(Error::Protocol(format!("daemon error {code:?}: {message}")))
            }
            Some(Ok(other)) => Err(Error::Protocol(format!("unexpected event: {other:?}"))),
            Some(Err(e)) => Err(e),
            None => Err(Error::Protocol(
                "connection closed before terminal event".into(),
            )),
        }
    }

    async fn request(&self, mut line: String) -> Result<EventStream, Error> {
        let mut stream = self.connect().await?;
        line.push('\n');
        stream.write_all(line.as_bytes()).await?;
        stream.flush().await?;
        Ok(EventStream {
            inner: Box::pin(events(BufReader::new(stream))),
        })
    }

    async fn connect(&self) -> Result<UnixStream, Error> {
        let deadline = Instant::now() + self.connect_timeout;
        let mut backoff = BACKOFF_START;
        loop {
            let err = match UnixStream::connect(&self.socket_path).await {
                Ok(s) => return Ok(s),
                Err(e) => e,
            };
            let retryable = matches!(
                err.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            );
            let now = Instant::now();
            if !retryable || now >= deadline {
                return Err(Error::DaemonUnreachable {
                    socket_path: self.socket_path.clone(),
                    source: err,
                });
            }
            tokio::time::sleep(backoff.min(deadline - now)).await;
            backoff = (backoff * 2).min(BACKOFF_CAP);
        }
    }
}

fn events(reader: BufReader<UnixStream>) -> impl Stream<Item = Result<Event, Error>> + Send {
    stream::unfold(Some(reader), |state| async move {
        let mut reader = state?;
        loop {
            let mut buf = String::new();
            match reader.read_line(&mut buf).await {
                Err(e) => return Some((Err(Error::Io(e)), None)),
                Ok(0) => {
                    let e = Error::Protocol("connection closed before terminal event".into());
                    return Some((Err(e), None));
                }
                Ok(_) => {}
            }
            if buf.trim().is_empty() {
                continue;
            }
            return Some(match serde_json::from_str::<Event>(&buf) {
                Ok(ev) => {
                    let next = if ev.is_terminal() { None } else { Some(reader) };
                    (Ok(ev), next)
                }
                Err(e) => (
                    Err(Error::Protocol(format!("invalid event line: {e}"))),
                    None,
                ),
            });
        }
    })
}
