use std::fmt;
use std::time::Duration;

use serde::Deserialize;

use crate::command::{CommandError, CommandSeq};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("RealityScan returned HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("unexpected response from RealityScan: {0}")]
    Decode(#[from] serde_json::Error),
    #[error(transparent)]
    Command(#[from] CommandError),
}

/// Identifies a command sequence submitted with [`Client::send`].
///
/// RealityScan formats these as 32 upper-case hex digits. Sequences the
/// application starts itself are reported with an all-zero ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct SequenceId(pub String);

impl SequenceId {
    pub fn is_internal(&self) -> bool {
        self.0.bytes().all(|b| b == b'0')
    }
}

impl fmt::Display for SequenceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Snapshot of what RealityScan is doing, from `GET /status`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Running process (see the process ID table), or -1 when idle.
    pub process_id: i64,
    /// 0.0..=1.0
    pub progress: f64,
    /// Seconds the current process has been running.
    pub time_total: f64,
    /// Estimated seconds remaining.
    pub time_estimation: f64,
    pub is_sequencer_empty: bool,
    pub is_master_busy: bool,
    pub is_background_busy: bool,
    /// Increments whenever the project changes; cheap way to know when to re-query state.
    pub change_counter: u64,
    /// HRESULT of the last error (0 when none). Reported unsigned here, signed in events.
    pub error_code: i64,
    pub error_message: String,
}

impl Status {
    pub fn is_idle(&self) -> bool {
        self.is_sequencer_empty && !self.is_master_busy && !self.is_background_busy
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CmdsHandle {
    sequence_id: SequenceId,
}

/// REST client for one RealityScan instance.
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
}

impl Client {
    /// `base` is the URL given to `-RsRemoteStartREST`, e.g. `http://127.0.0.1:30010`.
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client without TLS always builds");
        Self { base: base.into().trim_end_matches('/').to_owned(), http }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, route: &str) -> String {
        format!("{}{}", self.base, route)
    }

    async fn checked(resp: reqwest::Response) -> Result<reqwest::Response, Error> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(Error::Api { status: status.as_u16(), body })
        }
    }

    /// True if the server answers. Connection failures count as "not healthy"
    /// rather than errors, since that's the normal state while RealityScan starts.
    pub async fn health(&self) -> bool {
        match self.http.get(self.url("/health-check")).timeout(Duration::from_secs(5)).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// True once nothing accepts connections at the server's address any more.
    /// Unlike `!health()`, a slow or failing request doesn't count: RealityScan
    /// is only "gone" when the connection itself is refused.
    pub async fn is_gone(&self) -> bool {
        match self.http.get(self.url("/health-check")).timeout(Duration::from_secs(5)).send().await {
            Ok(_) => false,
            Err(e) => e.is_connect(),
        }
    }

    pub async fn status(&self) -> Result<Status, Error> {
        let resp = self.http.get(self.url("/status")).timeout(Duration::from_secs(10)).send().await?;
        let text = Self::checked(resp).await?.text().await?;
        Ok(serde_json::from_str(&crate::json::sanitize(&text))?)
    }

    /// Queue a command sequence. Returns as soon as RealityScan accepts it;
    /// follow progress through [`Client::status`] or events.
    pub async fn send(&self, seq: &CommandSeq) -> Result<SequenceId, Error> {
        self.send_raw(&seq.render()?).await
    }

    /// Queue a pre-rendered command string, e.g. `-newScene -align`.
    pub async fn send_raw(&self, cmds: &str) -> Result<SequenceId, Error> {
        let resp = self
            .http
            .post(self.url("/cmds"))
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(cmds.to_owned())
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        let text = Self::checked(resp).await?.text().await?;
        let handle: CmdsHandle = serde_json::from_str(&text)?;
        Ok(handle.sequence_id)
    }

    /// Evaluate report-template text against the current project and return the result.
    pub async fn template(&self, template: &str) -> Result<String, Error> {
        let resp = self
            .http
            .post(self.url("/template"))
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(template.to_owned())
            .timeout(Duration::from_secs(120))
            .send()
            .await?;
        Ok(Self::checked(resp).await?.text().await?)
    }

    /// `/abort`, `/pause` and `/resume` take no parameters but reject a bodiless request
    /// with HTTP 400: they want a JSON object, even an empty one.
    async fn post_empty(&self, route: &str) -> Result<(), Error> {
        let resp = self
            .http
            .post(self.url(route))
            .header("Content-Type", "application/json")
            .body("{}")
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        Self::checked(resp).await?;
        Ok(())
    }

    /// Abort the running process and clear queued commands.
    pub async fn abort(&self) -> Result<(), Error> {
        self.post_empty("/abort").await
    }

    pub async fn pause(&self) -> Result<(), Error> {
        self.post_empty("/pause").await
    }

    pub async fn resume(&self) -> Result<(), Error> {
        self.post_empty("/resume").await
    }

    /// Stop the REST server. RealityScan keeps running but can no longer be reached,
    /// so prefer sending `-quit` when shutting an instance down.
    pub async fn stop_server(&self) -> Result<(), Error> {
        self.post_empty("/stop").await
    }

    /// Poll `/status` until RealityScan is idle or `timeout` passes.
    pub async fn wait_idle(&self, poll: Duration, timeout: Duration) -> Result<Status, Error> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let status = self.status().await?;
            if status.is_idle() || tokio::time::Instant::now() >= deadline {
                return Ok(status);
            }
            tokio::time::sleep(poll).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_as_sent_by_realityscan() {
        // Verbatim shape from RealityScan 2.2, including its blank lines.
        let body = r#"{
    "processId":-1,
    "progress":0.000000,
    "timeTotal":9.955197,
    "timeEstimation":0.000000,

    "isSequencerEmpty":true,
    "isMasterBusy":false,
    "isBackgroundBusy":false,
    "changeCounter":2,

    "errorCode":2147942402,
    "errorMessage":"Failed to add image Z:\\nonexistent\\nope.jpg [err:7134]"
}"#;
        let s: Status = serde_json::from_str(body).unwrap();
        assert_eq!(s.process_id, -1);
        assert_eq!(s.change_counter, 2);
        assert_eq!(s.error_code, 2147942402);
        assert!(s.is_idle());
        assert!(s.error_message.contains("err:7134"));
    }

    #[test]
    fn internal_sequence_ids() {
        assert!(SequenceId("00000000000000000000000000000000".into()).is_internal());
        assert!(!SequenceId("CB196D8648A54833A0B1FEE8BBA40AB2".into()).is_internal());
    }
}
