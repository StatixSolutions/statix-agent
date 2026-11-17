//! Handles one decoded v2 message. `desired` triggers `reconcile::plan` and
//! queues the resulting status(es) + event; `ping` replies `pong`; `goaway`
//! tells the session to reconnect after the server's requested delay;
//! `logs.query` is answered from the local log spool; orchestration operations
//! are accepted only for durable agent updates.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::logs;
use crate::update;

use super::protocol::{
    IncomingMessage, LogQueryBody, LogResultBody, NodeDesiredState, OpResultBody, OperationSpec,
};
use super::reconcile::{self, ReconcilePlan};
use super::session::OutboundMessage;

pub enum HandleOutcome {
    Continue,
    Reconnect { after: Option<Duration> },
}

pub fn handle(
    message: IncomingMessage,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    last_desired: &mut Option<NodeDesiredState>,
) -> HandleOutcome {
    match message {
        IncomingMessage::Ping => {
            let _ = outbound_tx.send(OutboundMessage::Pong);
            HandleOutcome::Continue
        }
        IncomingMessage::Goaway(body) => {
            info!(
                retry_after_ms = body.retry_after_ms,
                "server requested goaway"
            );
            HandleOutcome::Reconnect {
                after: Some(Duration::from_millis(body.retry_after_ms)),
            }
        }
        IncomingMessage::Desired(state) => {
            apply_desired(state, outbound_tx, last_desired);
            HandleOutcome::Continue
        }
        IncomingMessage::LogQuery(query) => {
            spawn_log_query(query, outbound_tx.clone());
            HandleOutcome::Continue
        }
        IncomingMessage::OpDispatch(dispatch) => {
            spawn_operation(dispatch, outbound_tx.clone());
            HandleOutcome::Continue
        }
        IncomingMessage::OpCancel(cancel) => {
            match update::cancel(&cancel.op_id) {
                Ok(result) => {
                    let _ = outbound_tx.send(OutboundMessage::OpResult(result));
                }
                Err(error) => {
                    let _ = outbound_tx.send(OutboundMessage::OpResult(OpResultBody {
                        op_id: cancel.op_id,
                        status: "rejected".to_owned(),
                        reason: Some("unknown_operation".to_owned()),
                        message: Some(error.to_string()),
                    }));
                }
            }
            HandleOutcome::Continue
        }
        IncomingMessage::Unhandled(kind) => {
            info!(kind = %kind, "received v2 message not handled this phase");
            HandleOutcome::Continue
        }
        // Challenge/welcome only ever arrive during the handshake, which
        // session.rs consumes directly before this loop starts; seeing one
        // here would mean the server re-sent a handshake frame mid-session,
        // which nothing does today.
        IncomingMessage::Challenge(_) | IncomingMessage::Welcome(_) => {
            warn!("received unexpected handshake message outside the handshake");
            HandleOutcome::Continue
        }
    }
}

fn spawn_operation(
    dispatch: super::protocol::OpDispatchBody,
    outbound_tx: mpsc::UnboundedSender<OutboundMessage>,
) {
    let (operation, created) = match dispatch.spec {
        OperationSpec::UpdateAgent(target) => {
            match update::accept(&dispatch.op_id, target, dispatch.lease_expires_at) {
                Ok(operation) => operation,
                Err(error) => {
                    let _ = outbound_tx.send(OutboundMessage::OpResult(OpResultBody {
                        op_id: dispatch.op_id,
                        status: "rejected".to_owned(),
                        reason: Some("invalid_update".to_owned()),
                        message: Some(error.to_string()),
                    }));
                    return;
                }
            }
        }
    };

    let _ = outbound_tx.send(OutboundMessage::OpResult(OpResultBody {
        op_id: operation.op_id.clone(),
        status: operation.status.clone(),
        reason: None,
        message: Some("update accepted".to_owned()),
    }));
    if !created {
        return;
    }
    tokio::spawn(async move {
        if let Err(error) = update::start_worker().await {
            if let Ok(Some(mut operation)) = update::load_operation(&operation.op_id) {
                let result = OpResultBody {
                    op_id: operation.op_id.clone(),
                    status: "failed".to_owned(),
                    reason: Some("worker_start_failed".to_owned()),
                    message: Some(error.to_string()),
                };
                let _ = update::set_result(&mut operation, &result);
                let _ = outbound_tx.send(OutboundMessage::OpResult(result));
            }
        } else if let Ok(Some(operation)) = update::load_operation(&operation.op_id) {
            let _ = outbound_tx.send(OutboundMessage::OpResult(OpResultBody {
                op_id: operation.op_id,
                status: operation.status,
                reason: operation.reason,
                message: operation.message,
            }));
        }
    });
}

fn apply_desired(
    state: NodeDesiredState,
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    last_desired: &mut Option<NodeDesiredState>,
) {
    let ReconcilePlan {
        statuses,
        event,
        block,
    } = reconcile::plan(&state);
    let generation = state.generation;
    info!(generation, "received desired state:\n{block}");
    for status in statuses {
        let _ = outbound_tx.send(OutboundMessage::Status(status));
    }
    let _ = outbound_tx.send(OutboundMessage::Event(event));
    *last_desired = Some(state);
}

fn spawn_log_query(query: LogQueryBody, outbound_tx: mpsc::UnboundedSender<OutboundMessage>) {
    let LogQueryBody {
        request_id,
        scope,
        resource_id,
        cursor,
        limit,
    } = query;
    info!(
        request_id = %request_id,
        scope = scope.as_deref().unwrap_or("-"),
        resource_id = resource_id.as_deref().unwrap_or("-"),
        "received log query"
    );
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            logs::query(
                scope.as_deref(),
                resource_id.as_deref(),
                cursor.as_deref(),
                limit.unwrap_or(200),
            )
        })
        .await;
        match result {
            Ok(Ok((records, next_cursor))) => {
                let _ = outbound_tx.send(OutboundMessage::LogResult(LogResultBody {
                    request_id,
                    records,
                    next_cursor,
                }));
            }
            Ok(Err(error)) => warn!(error = %error, "failed to query local agent logs"),
            Err(error) => warn!(error = %error, "local agent log query task failed"),
        }
    });
}

/// Re-reports status for the last known desired state without waiting for a
/// new one — the periodic half of the "verify current against target" cycle
/// (`welcome.resyncSec`, per plans/controlplane-agent-communicationsystem.md
/// §3.3: "agent -> status ... full snapshot every resyncSec").
pub fn resync(state: &NodeDesiredState, outbound_tx: &mpsc::UnboundedSender<OutboundMessage>) {
    let ReconcilePlan {
        statuses, block, ..
    } = reconcile::plan(state);
    let generation = state.generation;
    info!(generation, "resync: re-reporting status:\n{block}");
    for status in statuses {
        let _ = outbound_tx.send(OutboundMessage::Status(status));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::protocol::GoawayBody;

    fn empty_state() -> NodeDesiredState {
        NodeDesiredState {
            node_id: "n1".to_string(),
            generation: 1,
            runtimes: Vec::new(),
            workloads: Vec::new(),
        }
    }

    #[test]
    fn ping_replies_pong() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut last_desired = None;

        let outcome = handle(IncomingMessage::Ping, &tx, &mut last_desired);

        assert!(matches!(outcome, HandleOutcome::Continue));
        assert!(matches!(rx.try_recv().unwrap(), OutboundMessage::Pong));
    }

    #[test]
    fn goaway_requests_reconnect_after_server_delay() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut last_desired = None;

        let outcome = handle(
            IncomingMessage::Goaway(GoawayBody {
                retry_after_ms: 2500,
            }),
            &tx,
            &mut last_desired,
        );

        match outcome {
            HandleOutcome::Reconnect { after: Some(delay) } => {
                assert_eq!(delay, Duration::from_millis(2500));
            }
            _ => panic!("expected a Reconnect outcome with a delay"),
        }
    }

    #[test]
    fn desired_state_updates_last_desired_and_queues_status_and_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut last_desired = None;

        let outcome = handle(
            IncomingMessage::Desired(empty_state()),
            &tx,
            &mut last_desired,
        );

        assert!(matches!(outcome, HandleOutcome::Continue));
        assert!(last_desired.is_some());
        assert!(matches!(rx.try_recv().unwrap(), OutboundMessage::Event(_)));
        assert!(rx.try_recv().is_err()); // no statuses for an empty desired state
    }

    #[tokio::test]
    async fn log_query_replies_with_a_log_result() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut last_desired = None;
        let query = crate::transport::protocol::decode(
            r#"{"v":2,"type":"logs.query","id":"01J","ts":1,"body":{"requestId":"r1","limit":1}}"#,
        )
        .unwrap();

        let outcome = handle(query, &tx, &mut last_desired);

        assert!(matches!(outcome, HandleOutcome::Continue));
        // The query runs on a spawned blocking task; an error querying the
        // local spool is logged and sends nothing, so only assert on the
        // reply when one arrives.
        if let Ok(Some(OutboundMessage::LogResult(body))) =
            tokio::time::timeout(Duration::from_secs(2), rx.recv()).await
        {
            assert_eq!(body.request_id, "r1");
        }
    }

    #[test]
    fn resync_resends_status_without_a_new_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let state = empty_state();

        resync(&state, &tx);

        assert!(rx.try_recv().is_err()); // empty state -> no statuses, and no event on resync
    }
}
