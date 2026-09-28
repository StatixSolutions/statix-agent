//! Wire messages exchanged with the control plane over `/api/ws/agent`.
//!
//! This is still the v1 shape (`auth`/`ready`/`error`/`job`/`log_query` one
//! way, `job_status`/`log_result`/`log_capabilities`/`metrics`/`system_info`
//! the other) — the only shape anything in the repo currently speaks.
//! `apps/node-controller/src/gateway/codec/v1.ts` describes this same shape
//! as "today's unchanged v1 agent protocol". The v2 envelope
//! `{ v, type, id, ack?, ts, body }` from
//! `plans/controlplane-agent-communicationsystem.md` §3.2 (see also
//! `apps/node-controller/src/gateway/codec/v2.ts`) is what this grows into
//! once node-controller's gateway actually terminates connections.

use serde::{Deserialize, Serialize};

use crate::{logs, metrics, system_info};

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ClientMessage<'a> {
    #[serde(rename = "auth")]
    Auth {
        #[serde(rename = "nodeId")]
        node_id: &'a str,
        #[serde(rename = "nodeToken")]
        node_token: &'a str,
        #[serde(rename = "logQuery")]
        log_query: bool,
    },
    #[serde(rename = "metrics")]
    Metrics {
        payload: &'a metrics::MetricsPayload,
    },
    #[serde(rename = "system_info")]
    SystemInfo {
        payload: &'a system_info::SystemInfoPayload,
    },
    #[serde(rename = "job_status")]
    JobStatus {
        #[serde(rename = "jobId")]
        job_id: &'a str,
        status: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<&'a str>,
    },
    #[serde(rename = "log_result")]
    LogResult {
        #[serde(rename = "requestId")]
        request_id: &'a str,
        records: &'a [logs::Record],
        #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
        next_cursor: Option<&'a str>,
    },
    #[serde(rename = "log_capabilities")]
    LogCapabilities,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    #[serde(rename = "ready")]
    Ready {
        #[serde(rename = "nodeId")]
        node_id: String,
    },
    #[serde(rename = "error")]
    Error { error: String },
    #[serde(rename = "job")]
    Job { job: AgentJob },
    #[serde(rename = "log_query")]
    LogQuery {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(default)]
        scope: Option<String>,
        #[serde(rename = "resourceId", default)]
        resource_id: Option<String>,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        limit: Option<usize>,
    },
}

/// The agent no longer interprets job specs itself — see `transport::dispatch`.
/// `spec` is kept as opaque JSON purely so it can be logged and its `kind`
/// field surfaced, instead of maintaining a full typed enum for every job
/// kind an execution engine we've disconnected used to understand.
#[derive(Debug, Deserialize)]
pub struct AgentJob {
    pub id: String,
    #[serde(rename = "issuedAt")]
    pub issued_at: u64,
    pub spec: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_job_message_with_opaque_spec() {
        let message: ServerMessage = serde_json::from_str(
            r#"{"type":"job","job":{"id":"j1","issuedAt":1,"spec":{"kind":"deploy_docker","projectId":"p1"}}}"#,
        )
        .unwrap();

        let ServerMessage::Job { job } = message else {
            panic!("expected a Job message");
        };
        assert_eq!(job.id, "j1");
        assert_eq!(
            job.spec.get("kind").and_then(|v| v.as_str()),
            Some("deploy_docker")
        );
    }

    #[test]
    fn encodes_job_status_with_message() {
        let encoded = serde_json::to_string(&ClientMessage::JobStatus {
            job_id: "j1",
            status: "failed",
            message: Some("not implemented"),
        })
        .unwrap();

        assert!(encoded.contains(r#""type":"job_status""#));
        assert!(encoded.contains(r#""jobId":"j1""#));
        assert!(encoded.contains(r#""message":"not implemented""#));
    }
}
