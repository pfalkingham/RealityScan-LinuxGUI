//! Push notifications from RealityScan.
//!
//! After `-RsRemoteStartNotifyREST <url>`, RealityScan POSTs a JSON object to
//! `<url>` for every sequence state change, process section start/end,
//! progress update and `-tag`. [`EventReceiver`] runs a tiny local HTTP server
//! to collect them.

use std::net::{Ipv4Addr, SocketAddr};

use axum::{Router, extract::State, routing::post};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::client::SequenceId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SequenceState {
    Running,
    Finished,
    Aborted,
    Other,
}

impl SequenceState {
    fn parse(s: &str) -> Self {
        match s {
            "Running" => Self::Running,
            "Finished" => Self::Finished,
            "Aborted" => Self::Aborted,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Sent once when notifications are enabled.
    HealthCheck,
    SequenceState {
        sequence_id: SequenceId,
        state: SequenceState,
        /// HRESULT; negative on failure.
        hr: i64,
        message: Option<String>,
        time: u64,
    },
    SectionStarted {
        /// Process ID, see the process ID table.
        algorithm_id: u32,
        level: u32,
    },
    SectionCompleted {
        algorithm_id: u32,
        level: u32,
        /// Seconds the section took.
        duration: f64,
        hr: i64,
        error: Option<String>,
    },
    /// 0.0..=1.0 for the running process.
    Progress(f64),
    Tag(String),
    /// Anything this crate doesn't know yet, kept for logging.
    Unknown(Value),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    #[serde(rename = "type")]
    kind: String,
    sequence_id: Option<SequenceId>,
    state: Option<String>,
    hr: Option<i64>,
    hr_msg: Option<String>,
    time: Option<u64>,
    algorithm_id: Option<String>,
    level: Option<u32>,
    d_time: Option<f64>,
    error_message: Option<String>,
    progress: Option<f64>,
    tag: Option<String>,
}

impl Event {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        let value: Value = serde_json::from_str(json)?;
        let Ok(raw) = Raw::deserialize(&value) else {
            return Ok(Self::Unknown(value));
        };
        let algorithm_id = raw.algorithm_id.as_deref().and_then(|s| s.parse().ok());
        let event = match raw.kind.as_str() {
            "HEALTH_CHECK_TEST_NOTIFY_REST" => Self::HealthCheck,
            "SEQUENCE_EXECUTION_STATE_UPDATE" => match (raw.sequence_id, raw.state) {
                (Some(sequence_id), Some(state)) => Self::SequenceState {
                    sequence_id,
                    state: SequenceState::parse(&state),
                    hr: raw.hr.unwrap_or(0),
                    message: raw.hr_msg.filter(|m| !m.is_empty()),
                    time: raw.time.unwrap_or(0),
                },
                _ => Self::Unknown(value),
            },
            "PEI_MAIN_EVENT_SECTION_STARTED" => match algorithm_id {
                Some(algorithm_id) => Self::SectionStarted { algorithm_id, level: raw.level.unwrap_or(0) },
                None => Self::Unknown(value),
            },
            "PEI_MAIN_EVENT_SECTION_COMPLETED" => match algorithm_id {
                Some(algorithm_id) => Self::SectionCompleted {
                    algorithm_id,
                    level: raw.level.unwrap_or(0),
                    duration: raw.d_time.unwrap_or(0.0),
                    hr: raw.hr.unwrap_or(0),
                    error: raw.error_message.filter(|m| !m.is_empty()),
                },
                None => Self::Unknown(value),
            },
            "SEQUENCE_EXECUTION_PROGRESS_UPDATE" => match raw.progress {
                Some(p) => Self::Progress(p),
                None => Self::Unknown(value),
            },
            "ON_TAG" => match raw.tag {
                Some(tag) => Self::Tag(tag),
                None => Self::Unknown(value),
            },
            _ => Self::Unknown(value),
        };
        Ok(event)
    }
}

/// Local HTTP endpoint that collects RealityScan's notifications.
///
/// Binds `127.0.0.1` on an ephemeral port; pass [`EventReceiver::url`] to
/// `-RsRemoteStartNotifyREST`. The server stops when the receiver is dropped.
pub struct EventReceiver {
    addr: SocketAddr,
    rx: mpsc::UnboundedReceiver<Event>,
    server: tokio::task::JoinHandle<()>,
}

const EVENTS_ROUTE: &str = "/events";

impl EventReceiver {
    pub async fn bind() -> std::io::Result<Self> {
        Self::bind_port(0).await
    }

    pub async fn bind_port(port: u16) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
        let addr = listener.local_addr()?;
        let (tx, rx) = mpsc::unbounded_channel();
        let app = Router::new().route(EVENTS_ROUTE, post(receive)).with_state(tx);
        let server = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                tracing::warn!("event receiver stopped: {e}");
            }
        });
        Ok(Self { addr, rx, server })
    }

    /// URL to hand to `-RsRemoteStartNotifyREST`.
    pub fn url(&self) -> String {
        format!("http://{}{}", self.addr, EVENTS_ROUTE)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Next event, or `None` if the server has stopped.
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        self.rx.try_recv().ok()
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn receive(State(tx): State<mpsc::UnboundedSender<Event>>, body: String) -> &'static str {
    match Event::parse(&body) {
        Ok(event) => {
            let _ = tx.send(event);
        }
        Err(e) => tracing::warn!("unparseable RealityScan event ({e}): {body}"),
    }
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sequence_state() {
        let e = Event::parse(
            r#"{"hr": -2147024894, "hrMsg": "Failed to add image Z:\\nonexistent\\nope.jpg [err:7134]",
                "sequenceId": "9252E5CC2FD744DDA3CFFEACE83CE519", "state": "Aborted",
                "time": 1791825921, "type": "SEQUENCE_EXECUTION_STATE_UPDATE"}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Event::SequenceState {
                sequence_id: SequenceId("9252E5CC2FD744DDA3CFFEACE83CE519".into()),
                state: SequenceState::Aborted,
                hr: -2147024894,
                message: Some(r"Failed to add image Z:\nonexistent\nope.jpg [err:7134]".into()),
                time: 1791825921,
            }
        );
    }

    #[test]
    fn parses_sections_progress_and_tags() {
        assert_eq!(
            Event::parse(r#"{"algorithmId": "65536", "dTime": 0.0, "hr": 0, "level": 2, "type": "PEI_MAIN_EVENT_SECTION_STARTED"}"#)
                .unwrap(),
            Event::SectionStarted { algorithm_id: 65536, level: 2 }
        );
        assert_eq!(
            Event::parse(
                r#"{"algorithmId": "4", "dTime": 0.2045086, "errorMessage": "boom", "hr": -5, "level": 0,
                    "type": "PEI_MAIN_EVENT_SECTION_COMPLETED"}"#
            )
            .unwrap(),
            Event::SectionCompleted {
                algorithm_id: 4,
                level: 0,
                duration: 0.2045086,
                hr: -5,
                error: Some("boom".into())
            }
        );
        assert_eq!(
            Event::parse(r#"{"progress": 0.25, "type": "SEQUENCE_EXECUTION_PROGRESS_UPDATE"}"#).unwrap(),
            Event::Progress(0.25)
        );
        assert_eq!(
            Event::parse(r#"{"tag": "ADDED", "type": "ON_TAG"}"#).unwrap(),
            Event::Tag("ADDED".into())
        );
        assert_eq!(Event::parse(r#"{"type":"HEALTH_CHECK_TEST_NOTIFY_REST"}"#).unwrap(), Event::HealthCheck);
    }

    #[test]
    fn keeps_unknown_events() {
        assert!(matches!(Event::parse(r#"{"type":"SOMETHING_NEW","x":1}"#).unwrap(), Event::Unknown(_)));
        assert!(Event::parse("not json").is_err());
    }

    #[tokio::test]
    async fn receiver_delivers_posted_events() {
        let mut rx = EventReceiver::bind().await.unwrap();
        let resp = reqwest::Client::new()
            .post(rx.url())
            .body(r#"{"tag": "DONE", "type": "ON_TAG"}"#)
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        assert_eq!(rx.recv().await, Some(Event::Tag("DONE".into())));
    }
}
