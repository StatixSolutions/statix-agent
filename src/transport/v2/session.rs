//! Protocol v2 session: connect, then `hello` -> await `challenge` -> `auth`
//! -> await `welcome` (plans/controlplane-agent-communicationsystem.md §3.3),
//! then the steady-state loop — reply to `ping`, honor `goaway`, hand
//! `desired` to `dispatch`, and fire a resync tick every `resyncSec` that
//! re-reports status even with nothing new. That resync tick is the
//! "verify current against target" cycle.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use sha2::{Digest, Sha256};
use tokio::{
    select,
    sync::{mpsc, watch},
    time::interval,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::dispatch::{self, HandleOutcome};
use super::protocol::{self, AuthBody, HelloBody, IncomingMessage, NodeDesiredState};
use crate::config::AgentConfig;

pub enum SessionOutcome {
    Stopped,
    /// The connection ended normally (error, close, or server `goaway`); the
    /// outer loop should reconnect, optionally after a server-requested
    /// delay instead of the usual jittered `reconnect_delay_ms`.
    Reconnect {
        after: Option<Duration>,
    },
}

#[derive(Debug)]
pub enum OutboundMessage {
    Pong,
    Status(protocol::ObjectStatus),
    Event(protocol::NodeEvent),
}

/// Runs the reconnect loop until `stop_rx` signals shutdown.
pub async fn run(config: &AgentConfig, mut stop_rx: watch::Receiver<bool>) -> Result<()> {
    let boot_id = protocol::new_id();

    while !*stop_rx.borrow() {
        let mut retry_after = None;
        match run_once(config, stop_rx.clone(), &boot_id).await {
            Ok(SessionOutcome::Stopped) => break,
            Ok(SessionOutcome::Reconnect { after }) => {
                info!("v2 session ended; reconnecting");
                retry_after = after;
            }
            Err(error) => {
                warn!(error = %error, "v2 session failed; reconnecting");
            }
        }

        if *stop_rx.borrow() {
            break;
        }

        let delay = retry_after.unwrap_or(Duration::from_millis(config.reconnect_delay_ms));
        select! {
            _ = tokio::time::sleep(delay) => {}
            changed = stop_rx.changed() => {
                if changed.is_ok() && *stop_rx.borrow() {
                    break;
                }
            }
        }
    }

    Ok(())
}

async fn run_once(
    config: &AgentConfig,
    mut stop_rx: watch::Receiver<bool>,
    boot_id: &str,
) -> Result<SessionOutcome> {
    let connect = tokio::time::timeout(
        Duration::from_millis(config.connect_timeout_ms),
        connect_async(config.agent_ws_url.as_str()),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "ws connect timed out after {} ms",
            config.connect_timeout_ms
        )
    })?
    .context("failed to connect websocket")?;
    let (mut ws, _) = connect;

    info!(websocket_url = %crate::redact_url(&config.agent_ws_url), "connected to server (protocol v2)");

    send(
        &mut ws,
        "hello",
        HelloBody {
            protocol: vec![2],
            agent_version: crate::system_info::agent_version(),
            boot_id: boot_id.to_string(),
            capabilities: Vec::new(), // honest: no execution capability yet
            observed: Default::default(), // no local desired-state store yet
            pending_op_results: Vec::new(),
        },
    )
    .await
    .context("failed to send hello")?;

    let challenge = await_message(
        &mut ws,
        config.connect_timeout_ms,
        |message| match message {
            IncomingMessage::Challenge(body) => Some(body),
            _ => None,
        },
    )
    .await
    .context("failed to receive challenge")?;
    debug!(nonce_len = challenge.nonce.len(), "received challenge");

    let signature = hex::encode(Sha256::digest(config.node_token.as_bytes()));
    send(
        &mut ws,
        "auth",
        AuthBody {
            node_id: config.node_id.clone(),
            signature,
        },
    )
    .await
    .context("failed to send auth")?;

    let welcome = await_message(
        &mut ws,
        config.connect_timeout_ms,
        |message| match message {
            IncomingMessage::Welcome(body) => Some(body),
            _ => None,
        },
    )
    .await
    .context("failed to receive welcome")?;

    if welcome.upgrade_required.unwrap_or(false) {
        return Err(anyhow!(
            "server requires an agent upgrade before v2 can proceed"
        ));
    }
    info!(
        session_id = %welcome.session_id,
        heartbeat_sec = welcome.heartbeat_sec,
        resync_sec = welcome.resync_sec,
        "v2 session established"
    );

    let (mut ws_write, mut ws_read) = ws.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<OutboundMessage>();
    tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            let payload = match message {
                OutboundMessage::Pong => protocol::encode("pong", serde_json::json!({})),
                OutboundMessage::Status(status) => protocol::encode("status", status),
                OutboundMessage::Event(event) => protocol::encode("event", event),
            };
            if ws_write.send(Message::Text(payload.into())).await.is_err() {
                warn!("outbound v2 message send failed");
                break;
            }
        }
    });

    let mut last_desired: Option<NodeDesiredState> = None;
    let mut resync_tick = interval(Duration::from_secs(u64::from(welcome.resync_sec)));
    resync_tick.tick().await; // first tick fires immediately; consume it

    loop {
        select! {
            incoming = ws_read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match protocol::decode(&text) {
                        Ok(message) => match dispatch::handle(message, &outbound_tx, &mut last_desired) {
                            HandleOutcome::Continue => {}
                            HandleOutcome::Reconnect { after } => {
                                return Ok(SessionOutcome::Reconnect { after });
                            }
                        },
                        Err(error) => {
                            debug!(?error, "ignored malformed v2 frame");
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => {
                    if *stop_rx.borrow() {
                        return Ok(SessionOutcome::Stopped);
                    }
                    let reason = frame
                        .map(|value| value.reason.to_string())
                        .unwrap_or_else(|| "websocket closed".to_owned());
                    return Err(anyhow!(reason));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    if *stop_rx.borrow() {
                        return Ok(SessionOutcome::Stopped);
                    }
                    return Err(error).context("websocket session failed");
                }
                None => {
                    if *stop_rx.borrow() {
                        return Ok(SessionOutcome::Stopped);
                    }
                    return Err(anyhow!("websocket connection ended"));
                }
            },
            _ = resync_tick.tick() => {
                if let Some(desired) = &last_desired {
                    dispatch::resync(desired, &outbound_tx);
                }
            }
            changed = stop_rx.changed() => {
                if changed.is_ok() && *stop_rx.borrow() {
                    return Ok(SessionOutcome::Stopped);
                }
            }
        }
    }
}

async fn send<T, S>(ws: &mut S, kind: &'static str, body: T) -> Result<()>
where
    T: serde::Serialize,
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let payload = protocol::encode(kind, body);
    ws.send(Message::Text(payload.into())).await?;
    Ok(())
}

/// Waits for the next incoming frame that `extract` recognizes, ignoring
/// (and logging) anything else — the v2 analog of v1 session.rs's
/// `await_ready`, tolerant of stray frames while waiting for one specific
/// handshake step.
async fn await_message<S, T>(
    ws: &mut S,
    timeout_ms: u64,
    mut extract: impl FnMut(IncomingMessage) -> Option<T>,
) -> Result<T>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match protocol::decode(&text) {
                    Ok(message) => {
                        if let Some(value) = extract(message) {
                            return Ok(value);
                        }
                    }
                    Err(error) => {
                        debug!(?error, "ignored malformed v2 frame during handshake");
                    }
                },
                Some(Ok(Message::Close(frame))) => {
                    let reason = frame
                        .map(|value| value.reason.to_string())
                        .unwrap_or_else(|| "websocket closed during handshake".to_owned());
                    return Err(anyhow!(reason));
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    return Err(anyhow!(error)).context("websocket handshake failed");
                }
                None => return Err(anyhow!("websocket closed before handshake completed")),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("v2 handshake timed out after {} ms", timeout_ms))?
}
