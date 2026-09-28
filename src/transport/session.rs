//! Connects to the control plane, authenticates, and reconnects with a
//! jittered-by-config delay when the socket drops. Owns outbound framing
//! (turning an [`OutboundMessage`] into a `ClientMessage` on the wire) and
//! the periodic metrics/system_info publish ticks. Incoming frames are
//! handed to [`super::dispatch::handle`].

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::{
    select,
    sync::{mpsc, watch},
    time::{MissedTickBehavior, interval},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, info, warn};

use super::dispatch::{self, HandleOutcome};
use super::protocol::{ClientMessage, ServerMessage};
use crate::config::{AgentConfig, WireGuardConfig};
use crate::{logs, metrics, system_info};

pub enum SessionOutcome {
    Stopped,
}

#[derive(Debug)]
pub enum OutboundMessage {
    JobStatus {
        job_id: String,
        status: &'static str,
        message: Option<String>,
    },
    LogResult {
        request_id: String,
        records: Vec<logs::Record>,
        next_cursor: Option<String>,
    },
    LogCapabilities,
    Metrics(metrics::MetricsPayload),
    SystemInfo(system_info::SystemInfoPayload),
}

/// Runs the reconnect loop until `stop_rx` signals shutdown.
pub async fn run(
    config: &AgentConfig,
    mut stop_rx: watch::Receiver<bool>,
    debug_log_only: bool,
) -> Result<()> {
    while !*stop_rx.borrow() {
        match run_once(config, stop_rx.clone(), debug_log_only).await {
            Ok(SessionOutcome::Stopped) => break,
            Err(error) => {
                warn!(error = %error, "websocket session failed; reconnecting");
            }
        }

        if *stop_rx.borrow() {
            break;
        }

        select! {
            _ = tokio::time::sleep(Duration::from_millis(config.reconnect_delay_ms)) => {}
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
    debug_log_only: bool,
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

    info!(websocket_url = %crate::redact_url(&config.agent_ws_url), "connected to server");

    send_client_message(
        &mut ws,
        &ClientMessage::Auth {
            node_id: &config.node_id,
            node_token: &config.node_token,
            log_query: true,
        },
    )
    .await
    .context("failed to send websocket auth")?;

    await_ready(&mut ws, config.connect_timeout_ms, &config.node_id).await?;

    let mut last_system_info_hash: Option<String> = None;
    let mut last_system_info_published_at: Option<Instant> = None;

    if let Err(error) = send_initial_metrics(&mut ws).await {
        warn!(error = %error, "initial metrics publish failed");
    }

    if let Err(error) = send_initial_system_info(
        &mut ws,
        config.wireguard.as_ref(),
        &mut last_system_info_hash,
        &mut last_system_info_published_at,
    )
    .await
    {
        warn!(error = %error, "initial system info publish failed");
    }

    let mut publish_tick = interval(Duration::from_millis(config.publish_interval_ms));
    let mut system_tick = interval(Duration::from_millis(config.system_info_check_interval_ms));
    publish_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    system_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    publish_tick.tick().await;
    system_tick.tick().await;

    let (mut ws_write, mut ws_read) = ws.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<OutboundMessage>();
    tokio::spawn(async move {
        while let Some(message) = outbound_rx.recv().await {
            let send_result = match message {
                OutboundMessage::JobStatus {
                    job_id,
                    status,
                    message,
                } => {
                    send_client_message(
                        &mut ws_write,
                        &ClientMessage::JobStatus {
                            job_id: &job_id,
                            status,
                            message: message.as_deref(),
                        },
                    )
                    .await
                }
                OutboundMessage::LogResult {
                    request_id,
                    records,
                    next_cursor,
                } => {
                    send_client_message(
                        &mut ws_write,
                        &ClientMessage::LogResult {
                            request_id: &request_id,
                            records: &records,
                            next_cursor: next_cursor.as_deref(),
                        },
                    )
                    .await
                }
                OutboundMessage::LogCapabilities => {
                    send_client_message(&mut ws_write, &ClientMessage::LogCapabilities).await
                }
                OutboundMessage::Metrics(payload) => {
                    send_client_message(
                        &mut ws_write,
                        &ClientMessage::Metrics { payload: &payload },
                    )
                    .await
                }
                OutboundMessage::SystemInfo(payload) => {
                    send_client_message(
                        &mut ws_write,
                        &ClientMessage::SystemInfo { payload: &payload },
                    )
                    .await
                }
            };

            if let Err(error) = send_result {
                warn!(error = %error, "outbound message send failed");
                break;
            }
        }
    });

    loop {
        select! {
            incoming = ws_read.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ServerMessage>(&text) {
                        Ok(message) => match dispatch::handle(message, &outbound_tx, debug_log_only) {
                            HandleOutcome::Continue => {}
                            HandleOutcome::Fatal(error) => return Err(error),
                        },
                        Err(_) => {
                            debug!(payload = %truncate_for_log(&text, 200), "ignored non-server-message websocket payload");
                        }
                    }
                }
                Some(Ok(Message::Close(frame))) => {
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
            _ = publish_tick.tick() => {
                if let Err(error) = publish_metrics_once(&outbound_tx).await {
                    warn!(error = %error, "metrics publish failed");
                }
            }
            _ = system_tick.tick() => {
                if let Err(error) = publish_system_info_if_needed(
                    &outbound_tx,
                    false,
                    config.wireguard.as_ref(),
                    config.system_info_republish_interval_ms,
                    &mut last_system_info_hash,
                    &mut last_system_info_published_at,
                ).await {
                    warn!(error = %error, "system info publish failed");
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

async fn await_ready<S>(ws: &mut S, timeout_ms: u64, node_id: &str) -> Result<()>
where
    S: Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let ready = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<ServerMessage>(&text)
                {
                    Ok(ServerMessage::Ready {
                        node_id: ready_node_id,
                    }) if ready_node_id == node_id => {
                        return Ok(());
                    }
                    Ok(ServerMessage::Ready {
                        node_id: ready_node_id,
                    }) => {
                        anyhow::bail!(
                            "websocket authenticated for unexpected node: {ready_node_id}"
                        );
                    }
                    Ok(ServerMessage::Error { error }) => {
                        anyhow::bail!("server error: {error}");
                    }
                    // The short-lived authentication probe only waits for
                    // `ready`; a normal session handles jobs and log queries.
                    Ok(ServerMessage::Job { .. }) | Ok(ServerMessage::LogQuery { .. }) | Err(_) => {
                    }
                },
                Some(Ok(Message::Close(frame))) => {
                    let reason = frame
                        .map(|value| value.reason.to_string())
                        .unwrap_or_else(|| "websocket closed during auth".to_owned());
                    anyhow::bail!(reason);
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(anyhow!(error)).context("websocket auth failed"),
                None => anyhow::bail!("websocket closed before ready"),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("websocket auth timed out after {} ms", timeout_ms))?;

    ready
}

async fn send_client_message<S>(ws: &mut S, message: &ClientMessage<'_>) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let payload = serde_json::to_string(message)?;
    ws.send(Message::Text(payload.into())).await?;
    Ok(())
}

async fn publish_metrics_once(outbound_tx: &mpsc::UnboundedSender<OutboundMessage>) -> Result<()> {
    let payload = metrics::collect_metrics()?;
    outbound_tx
        .send(OutboundMessage::Metrics(payload))
        .map_err(|_| anyhow!("metrics channel closed"))
        .context("failed to queue metrics payload")?;
    debug!("metrics payload queued");
    Ok(())
}

async fn send_initial_metrics<S>(ws: &mut S) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let payload = metrics::collect_metrics()?;
    send_client_message(ws, &ClientMessage::Metrics { payload: &payload })
        .await
        .context("failed to publish metrics payload")?;
    debug!("initial metrics payload published");
    Ok(())
}

async fn publish_system_info_if_needed(
    outbound_tx: &mpsc::UnboundedSender<OutboundMessage>,
    force: bool,
    wireguard: Option<&WireGuardConfig>,
    republish_interval_ms: u64,
    last_hash: &mut Option<String>,
    last_published_at: &mut Option<Instant>,
) -> Result<()> {
    let payload = system_info::collect_system_info(wireguard).await?;
    let freshness_due = last_published_at
        .map(|instant| instant.elapsed() >= Duration::from_millis(republish_interval_ms))
        .unwrap_or(true);
    let changed = last_hash.as_ref() != Some(&payload.hash);

    if force || changed || freshness_due {
        let hash = payload.hash.clone();
        outbound_tx
            .send(OutboundMessage::SystemInfo(payload))
            .map_err(|_| anyhow!("system info channel closed"))
            .context("failed to queue system info payload")?;
        *last_hash = Some(hash);
        *last_published_at = Some(Instant::now());
        debug!(changed, freshness_due, "system info payload queued");
    }

    Ok(())
}

async fn send_initial_system_info<S>(
    ws: &mut S,
    wireguard: Option<&WireGuardConfig>,
    last_hash: &mut Option<String>,
    last_published_at: &mut Option<Instant>,
) -> Result<()>
where
    S: Sink<Message> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let payload = system_info::collect_system_info(wireguard).await?;
    send_client_message(ws, &ClientMessage::SystemInfo { payload: &payload })
        .await
        .context("failed to publish system info payload")?;
    *last_hash = Some(payload.hash.clone());
    *last_published_at = Some(Instant::now());
    debug!("initial system info payload published");
    Ok(())
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut shortened = value.chars().take(max_chars).collect::<String>();
    shortened.push_str("...");
    shortened
}
