//! Persistent desired-state runtime reconciliation.

use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use tokio::{process::Command, sync::Mutex};
use tracing::warn;

use crate::{
    jobs::runners::container::{
        image::LxcImage,
        lxc::{self, LxcContainer},
    },
    logs,
};

use super::{
    protocol::{
        Condition, ConditionStatus, EventSeverity, NodeDesiredState, NodeEvent, ObjectKind,
        ObjectStatus, Phase, PowerState, RuntimeSpec, RuntimeType, new_id, now_rfc3339,
    },
    session::OutboundMessage,
};

const RECONCILE_TIMEOUT_SECONDS: u64 = 600;

#[derive(Clone, Default)]
pub struct RuntimeController {
    state: Arc<Mutex<ControllerState>>,
}

#[derive(Default)]
struct ControllerState {
    managed: HashMap<String, String>,
}

impl RuntimeController {
    pub fn reconcile(
        &self,
        desired: NodeDesiredState,
        outbound: tokio::sync::mpsc::UnboundedSender<OutboundMessage>,
    ) {
        let state = self.state.clone();
        let generation = desired.generation;
        let task = async move {
            let mut guard = state.lock().await;
            if let Err(error) = reconcile_locked(&mut guard, &desired, &outbound).await {
                warn!(error = %error, generation = desired.generation, "runtime reconciliation failed");
            }
        };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(task);
        } else {
            warn!(
                generation,
                "runtime reconciliation skipped outside a Tokio runtime"
            );
        }
    }
}

async fn reconcile_locked(
    controller: &mut ControllerState,
    desired: &NodeDesiredState,
    outbound: &tokio::sync::mpsc::UnboundedSender<OutboundMessage>,
) -> Result<()> {
    let desired_ids: HashSet<&str> = desired
        .runtimes
        .iter()
        .map(|runtime| runtime.meta.id.as_str())
        .collect();
    let desired_names: HashSet<String> = desired
        .runtimes
        .iter()
        .map(|runtime| runtime_name(&runtime.meta.id))
        .collect();
    let mut stale = controller
        .managed
        .iter()
        .filter(|(id, _)| !desired_ids.contains(id.as_str()))
        .map(|(id, name)| (id.clone(), name.clone()))
        .collect::<Vec<_>>();
    let known_names: HashSet<String> = controller.managed.values().cloned().collect();
    for name in lxc::runtime_names() {
        if known_names.contains(&name) || desired_names.contains(&name) {
            continue;
        }
        let id = name.trim_start_matches("statix-runtime-").to_owned();
        stale.push((id, name));
    }

    for (id, name) in stale {
        let mut container = LxcContainer::existing(name.clone());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(RECONCILE_TIMEOUT_SECONDS),
            container.destroy(),
        )
        .await
        .context("timed out destroying stale runtime")?;
        match result {
            Ok(()) => {
                controller.managed.remove(&id);
                lifecycle(
                    &id,
                    "runtime_destroy",
                    "succeeded",
                    None,
                    "removed from desired state",
                );
                emit_event(
                    outbound,
                    desired,
                    Some(id),
                    EventSeverity::Info,
                    "RuntimeDestroyed",
                    "runtime removed from desired state",
                );
            }
            Err(error) => {
                lifecycle(&id, "runtime_destroy", "failed", None, &error.to_string());
                emit_event(
                    outbound,
                    desired,
                    Some(id),
                    EventSeverity::Error,
                    "RuntimeDestroyFailed",
                    &error.to_string(),
                );
            }
        }
    }

    for runtime in &desired.runtimes {
        let status = reconcile_runtime(controller, runtime).await;
        let _ = outbound.send(OutboundMessage::Status(status));
    }
    Ok(())
}

async fn reconcile_runtime(
    controller: &mut ControllerState,
    runtime: &RuntimeSpec,
) -> ObjectStatus {
    if !matches!(runtime.spec.runtime_type, RuntimeType::Lxc) {
        return unsupported_status(
            runtime,
            "MicroVM desired-state execution is not implemented in v0.18.0",
        );
    }

    let name = runtime_name(&runtime.meta.id);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(RECONCILE_TIMEOUT_SECONDS),
        reconcile_lxc(&name, runtime),
    )
    .await;
    match result {
        Ok(Ok(())) => {
            controller
                .managed
                .insert(runtime.meta.id.clone(), name.clone());
            let _ = logs::append_lifecycle(
                "info",
                "runtime",
                Some(&runtime.meta.id),
                None,
                None,
                "runtime_execution",
                "succeeded",
                None,
                &format!("container={name}"),
            );
            ready_status(runtime, &name)
        }
        Ok(Err(error)) => {
            let message = error.to_string();
            let _ = logs::append_lifecycle(
                "error",
                "runtime",
                Some(&runtime.meta.id),
                None,
                None,
                "runtime_execution",
                "failed",
                None,
                &message,
            );
            failed_status(runtime, &message)
        }
        Err(_) => failed_status(runtime, "runtime reconciliation timed out"),
    }
}

async fn reconcile_lxc(name: &str, runtime: &RuntimeSpec) -> Result<()> {
    let image = LxcImage::parse(&runtime.spec.image)?;
    let cpu = runtime.spec.resources.cpu.ceil();
    if !(1.0..=255.0).contains(&cpu) {
        bail!("cpu must be between 1 and 255");
    }
    let memory = runtime.spec.resources.memory_mb;
    if memory == 0 || memory > u64::from(u32::MAX) {
        bail!("memoryMb must be between 1 and {}", u32::MAX);
    }

    let path = lxc::runtime_config_path(name);
    let mut container = if path.exists() {
        LxcContainer::existing(name.to_owned())
    } else {
        lifecycle(
            &runtime.meta.id,
            "runtime_create",
            "started",
            None,
            &format!("container={name} image={}", runtime.spec.image),
        );
        LxcContainer::create(
            name.to_owned(),
            &image.distribution,
            &image.release,
            cpu as u8,
            memory as u32,
        )
        .await?
    };

    lxc::configure_runtime(name, cpu as u32, memory as u32)?;
    if matches!(runtime.spec.power, PowerState::Running) {
        let state = container.state().await.unwrap_or_default();
        if state != "RUNNING" {
            container.start().await?;
            container.configure_guest_network(60).await?;
            container.configure_guest_dns(60).await?;
        }
        if let Some(console) = &runtime.spec.console {
            let public_key = derive_public_key(&console.identity_file).await?;
            container
                .provision_ssh(&console.user, &public_key, 300)
                .await?;
        }
    } else if container.state().await.unwrap_or_default() == "RUNNING" {
        container.stop().await?;
    }
    Ok(())
}

async fn derive_public_key(identity_file: &str) -> Result<String> {
    let output = Command::new("ssh-keygen")
        .args(["-y", "-f", identity_file])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to run ssh-keygen")?;
    if !output.status.success() {
        bail!(
            "failed to derive public key from {}: {}",
            identity_file,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let key = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if key.is_empty() {
        bail!("ssh-keygen returned an empty public key");
    }
    Ok(key)
}

pub(crate) fn runtime_name(id: &str) -> String {
    let suffix = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!(
        "statix-runtime-{}",
        suffix.chars().take(52).collect::<String>()
    )
}

fn ready_status(runtime: &RuntimeSpec, name: &str) -> ObjectStatus {
    ObjectStatus {
        id: runtime.meta.id.clone(),
        kind: ObjectKind::Runtime,
        node_id: runtime.meta.node_id.clone(),
        observed_generation: runtime.meta.generation,
        phase: if matches!(runtime.spec.power, PowerState::Running) {
            Phase::Ready
        } else {
            Phase::Pending
        },
        conditions: vec![Condition {
            condition_type: "Available".into(),
            status: if matches!(runtime.spec.power, PowerState::Running) {
                ConditionStatus::True
            } else {
                ConditionStatus::False
            },
            reason: Some(
                if matches!(runtime.spec.power, PowerState::Running) {
                    "Running"
                } else {
                    "Stopped"
                }
                .into(),
            ),
            message: Some(format!("persistent LXC runtime {name} reconciled")),
            last_transition_at: now_rfc3339(),
        }],
        services: None,
        last_error: None,
        reported_at: now_rfc3339(),
    }
}

fn failed_status(runtime: &RuntimeSpec, message: &str) -> ObjectStatus {
    ObjectStatus {
        id: runtime.meta.id.clone(),
        kind: ObjectKind::Runtime,
        node_id: runtime.meta.node_id.clone(),
        observed_generation: runtime.meta.generation,
        phase: Phase::Failed,
        conditions: vec![Condition {
            condition_type: "Available".into(),
            status: ConditionStatus::False,
            reason: Some("ExecutionFailed".into()),
            message: Some(message.into()),
            last_transition_at: now_rfc3339(),
        }],
        services: None,
        last_error: Some(super::protocol::LastError {
            reason: "ExecutionFailed".into(),
            message: message.into(),
            at: now_rfc3339(),
        }),
        reported_at: now_rfc3339(),
    }
}

fn unsupported_status(runtime: &RuntimeSpec, message: &str) -> ObjectStatus {
    ObjectStatus {
        id: runtime.meta.id.clone(),
        kind: ObjectKind::Runtime,
        node_id: runtime.meta.node_id.clone(),
        observed_generation: runtime.meta.generation,
        phase: Phase::Pending,
        conditions: vec![Condition {
            condition_type: "Available".into(),
            status: ConditionStatus::False,
            reason: Some("Unsupported".into()),
            message: Some(message.into()),
            last_transition_at: now_rfc3339(),
        }],
        services: None,
        last_error: None,
        reported_at: now_rfc3339(),
    }
}

fn lifecycle(id: &str, step: &str, outcome: &str, duration_ms: Option<u64>, message: &str) {
    let _ = logs::append_lifecycle(
        "info",
        "runtime",
        Some(id),
        None,
        None,
        step,
        outcome,
        duration_ms,
        message,
    );
}

#[cfg(test)]
mod tests {
    use super::runtime_name;

    #[test]
    fn runtime_names_are_stable_and_lxc_safe() {
        assert_eq!(
            runtime_name("01M84B4KQMQT71WK9AJW4RY1YK"),
            "statix-runtime-01m84b4kqmqt71wk9ajw4ry1yk"
        );
        assert!(
            runtime_name("runtime/with spaces")
                .chars()
                .all(|c| { c.is_ascii_alphanumeric() || c == '-' || c == '_' })
        );
    }
}

fn emit_event(
    outbound: &tokio::sync::mpsc::UnboundedSender<OutboundMessage>,
    desired: &NodeDesiredState,
    object_id: Option<String>,
    severity: EventSeverity,
    reason: &str,
    message: &str,
) {
    let _ = outbound.send(OutboundMessage::Event(NodeEvent {
        id: new_id(),
        node_id: desired.node_id.clone(),
        project_id: None,
        object_id,
        generation: Some(desired.generation),
        severity,
        reason: reason.into(),
        message: message.into(),
        at: now_rfc3339(),
    }));
}
