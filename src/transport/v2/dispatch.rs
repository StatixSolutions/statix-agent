//! Handles one decoded v2 message. `desired` triggers `reconcile::plan` and
//! queues the resulting status(es) + event; `ping` replies `pong`; `goaway`
//! tells the session to reconnect after the server's requested delay;
//! anything else (`op_dispatch`/`op_cancel`/`logs.*`) is logged and dropped —
//! mirroring node-controller's own gateway, whose `handleV2` default case
//! does the same for frames *from* the agent it doesn't handle yet.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use super::protocol::{IncomingMessage, NodeDesiredState};
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
    use crate::transport::v2::protocol::GoawayBody;

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

    #[test]
    fn resync_resends_status_without_a_new_event() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let state = empty_state();

        resync(&state, &tx);

        assert!(rx.try_recv().is_err()); // empty state -> no statuses, and no event on resync
    }
}
