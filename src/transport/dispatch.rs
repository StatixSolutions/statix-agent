//! Turns one incoming [`ServerMessage`] into a log line and, for message
//! kinds that expect a reply, the outbound reply to send.
//!
//! The agent no longer executes jobs itself — see
//! `plans/controlplane-agent-communicationsystem.md`. A `Job` is logged and
//! explicitly rejected via `job_status`, rather than silently dropped or left
//! to hang forever in whatever state the server last saw it in (the design
//! doc's §3.3 principle: reply `rejected`/explicit-failure to anything the
//! agent won't act on, never drop it silently).

use anyhow::anyhow;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::intent::{self, JobIntent};
use super::protocol::{AgentJob, ServerMessage};
use super::session::OutboundMessage;
use crate::logs;

const NOT_IMPLEMENTED_MESSAGE: &str = "not implemented: agent job execution is being rebuilt, see plans/controlplane-agent-communicationsystem.md";

pub enum HandleOutcome {
    Continue,
    Fatal(anyhow::Error),
}

pub fn handle(
    message: ServerMessage,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    debug_log_only: bool,
) -> HandleOutcome {
    match message {
        ServerMessage::Ready { node_id } => {
            debug!(node_id = %node_id, "server ready");
            let _ = outbound_tx.send(OutboundMessage::LogCapabilities);
            HandleOutcome::Continue
        }
        ServerMessage::Error { error } => HandleOutcome::Fatal(anyhow!("server error: {error}")),
        ServerMessage::Job { job } => {
            handle_job(job, outbound_tx, debug_log_only);
            HandleOutcome::Continue
        }
        ServerMessage::LogQuery {
            request_id,
            scope,
            resource_id,
            cursor,
            limit,
        } => {
            spawn_log_query(
                request_id,
                scope,
                resource_id,
                cursor,
                limit,
                outbound_tx.clone(),
            );
            HandleOutcome::Continue
        }
    }
}

/// Logs exactly what this job would do, then replies without ever executing
/// it. Job execution is disconnected regardless of `debug_log_only` today
/// (see plans/controlplane-agent-communicationsystem.md); the flag only
/// changes the reply's wording, so the interface is ready for whenever a
/// real execution path is reconnected: `if debug_log_only { describe-only }
/// else { execute }`.
fn handle_job(
    job: AgentJob,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    debug_log_only: bool,
) {
    let JobIntent {
        kind,
        summary,
        fields,
    } = intent::describe(&job.spec);
    info!(
        job_id = %job.id,
        issued_at = job.issued_at,
        kind = %kind,
        summary = %summary,
        fields = ?fields,
        debug_log_only,
        "received job (not executed)"
    );

    let message = if debug_log_only {
        format!("debug-log-only: logged intended action for '{kind}', not executed")
    } else {
        NOT_IMPLEMENTED_MESSAGE.to_string()
    };
    let _ = outbound_tx.send(OutboundMessage::JobStatus {
        job_id: job.id,
        status: "failed",
        message: Some(message),
    });
}

fn spawn_log_query(
    request_id: String,
    scope: Option<String>,
    resource_id: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
    outbound_tx: mpsc::UnboundedSender<OutboundMessage>,
) {
    info!(
        request_id = %request_id,
        scope = scope.as_deref().unwrap_or("-"),
        resource_id = resource_id.as_deref().unwrap_or("-"),
        "received log query"
    );
    tokio::spawn(async move {
        let query = tokio::task::spawn_blocking(move || {
            logs::query(
                scope.as_deref(),
                resource_id.as_deref(),
                cursor.as_deref(),
                limit.unwrap_or(200),
            )
        })
        .await;
        match query {
            Ok(Ok((records, next_cursor))) => {
                let _ = outbound_tx.send(OutboundMessage::LogResult {
                    request_id,
                    records,
                    next_cursor,
                });
            }
            Ok(Err(error)) => warn!(error = %error, "failed to query local agent logs"),
            Err(error) => warn!(error = %error, "local agent log query task failed"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(spec: serde_json::Value) -> ServerMessage {
        ServerMessage::Job {
            job: AgentJob {
                id: "j1".to_string(),
                issued_at: 1,
                spec,
            },
        }
    }

    #[test]
    fn job_is_logged_and_rejected_not_executed() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let outcome = handle(
            job(serde_json::json!({"kind": "deploy_docker"})),
            &tx,
            false,
        );

        assert!(matches!(outcome, HandleOutcome::Continue));
        match rx.try_recv().unwrap() {
            OutboundMessage::JobStatus {
                job_id,
                status,
                message,
            } => {
                assert_eq!(job_id, "j1");
                assert_eq!(status, "failed");
                assert_eq!(message.as_deref(), Some(NOT_IMPLEMENTED_MESSAGE));
            }
            other => panic!("expected a JobStatus reply, got {other:?}"),
        }
    }

    #[test]
    fn job_is_logged_and_rejected_with_debug_log_only_wording() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let outcome = handle(job(serde_json::json!({"kind": "deploy_docker"})), &tx, true);

        assert!(matches!(outcome, HandleOutcome::Continue));
        match rx.try_recv().unwrap() {
            OutboundMessage::JobStatus {
                status, message, ..
            } => {
                assert_eq!(status, "failed");
                let message = message.unwrap();
                assert!(message.starts_with("debug-log-only:"));
                assert!(message.contains("deploy_docker"));
            }
            other => panic!("expected a JobStatus reply, got {other:?}"),
        }
    }

    #[test]
    fn ready_requests_log_capabilities() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let outcome = handle(
            ServerMessage::Ready {
                node_id: "n1".to_string(),
            },
            &tx,
            false,
        );

        assert!(matches!(outcome, HandleOutcome::Continue));
        assert!(matches!(
            rx.try_recv().unwrap(),
            OutboundMessage::LogCapabilities
        ));
    }

    #[test]
    fn server_error_is_fatal() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = handle(
            ServerMessage::Error {
                error: "boom".to_string(),
            },
            &tx,
            false,
        );

        assert!(matches!(outcome, HandleOutcome::Fatal(_)));
    }
}
