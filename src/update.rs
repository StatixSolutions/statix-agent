//! Durable orchestration-issued agent updates.
//!
//! The agent owns only the execution of an explicitly addressed update. The
//! orchestration plane owns rollout timing, targeting, and retry policy.

use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;

use crate::config::agent_state_dir;
use crate::transport::protocol::{now_rfc3339, Id, OpResultBody, UpdateAgentSpec};

const PACKAGE: &str = "statix-agent-runtime";
const NIX_FEATURES: [&str; 2] = ["--extra-experimental-features", "nix-command flakes"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOperation {
    pub op_id: Id,
    pub target: UpdateAgentSpec,
    pub lease_expires_at: String,
    pub status: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    pub updated_at: String,
}

fn operations_dir() -> Result<PathBuf> {
    let path = agent_state_dir()?.join("updates");
    fs::create_dir_all(&path)
        .with_context(|| format!("failed to create update state directory {}", path.display()))?;
    Ok(path)
}

fn operation_path(op_id: &str) -> Result<PathBuf> {
    let path = Path::new(op_id);
    if op_id.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || path.file_name().and_then(|name| name.to_str()) != Some(op_id)
    {
        bail!("invalid operation id");
    }
    Ok(operations_dir()?.join(format!("{op_id}.json")))
}

fn operation_lock() -> Result<File> {
    let path = operations_dir()?.join("operations.lock");
    let lock = File::create(path).context("failed to create update operation lock")?;
    lock.lock_exclusive()
        .context("failed to lock update operations")?;
    Ok(lock)
}

fn write_operation(operation: &UpdateOperation) -> Result<()> {
    let path = operation_path(&operation.op_id)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(operation)?)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .with_context(|| format!("failed to activate {}", path.display()))?;
    Ok(())
}

pub fn load_operation(op_id: &str) -> Result<Option<UpdateOperation>> {
    let path = operation_path(op_id)?;
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(Some(
        serde_json::from_slice(&bytes).context("invalid update operation state")?,
    ))
}

pub fn pending_results() -> Result<Vec<OpResultBody>> {
    let directory = operations_dir()?;
    let mut results = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(?error, path = %path.display(), "skipping unreadable update state");
                continue;
            }
        };
        let operation: UpdateOperation = match serde_json::from_slice(&bytes) {
            Ok(operation) => operation,
            Err(error) => {
                tracing::warn!(?error, path = %path.display(), "skipping invalid update state");
                continue;
            }
        };
        results.push(OpResultBody {
            op_id: operation.op_id,
            status: operation.status,
            reason: operation.reason,
            message: operation.message,
        });
    }
    results.sort_by(|left, right| left.op_id.cmp(&right.op_id));
    Ok(results)
}

pub fn accept(
    op_id: &str,
    target: UpdateAgentSpec,
    lease_expires_at: String,
) -> Result<(UpdateOperation, bool)> {
    let _lock = operation_lock()?;
    if target.flake_ref.trim().is_empty() || target.revision.trim().is_empty() {
        bail!("update requires an immutable flakeRef and revision");
    }
    let expires = time::OffsetDateTime::parse(
        &lease_expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .context("update lease is not a valid RFC3339 timestamp")?;
    if expires <= time::OffsetDateTime::now_utc() {
        bail!("update operation lease has expired");
    }
    if let Some(existing) = load_operation(op_id)? {
        return Ok((existing, false));
    }
    let operation = UpdateOperation {
        op_id: op_id.to_owned(),
        target,
        lease_expires_at,
        status: "accepted".to_owned(),
        reason: None,
        message: None,
        updated_at: now_rfc3339(),
    };
    write_operation(&operation)?;
    Ok((operation, true))
}

pub fn set_result(operation: &mut UpdateOperation, result: &OpResultBody) -> Result<()> {
    let _lock = operation_lock()?;
    operation.status = result.status.clone();
    operation.reason = result.reason.clone();
    operation.message = result.message.clone();
    operation.updated_at = now_rfc3339();
    write_operation(operation)
}

pub async fn start_worker() -> Result<()> {
    let output = TokioCommand::new("systemctl")
        .args(["start", "statix-agent-update.service"])
        .output()
        .await
        .context("failed to start update worker")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let fallback = TokioCommand::new("sudo")
        .args(["-n", "systemctl", "start", "statix-agent-update.service"])
        .output()
        .await
        .context("failed to start update worker through sudo")?;
    if !fallback.status.success() {
        bail!("update worker failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn cancel(op_id: &str) -> Result<OpResultBody> {
    let _lock = operation_lock()?;
    let mut operation = load_operation(op_id)?.context("unknown update operation")?;
    if operation.status == "accepted" {
        let result = OpResultBody {
            op_id: op_id.to_owned(),
            status: "rejected".to_owned(),
            reason: Some("cancelled".to_owned()),
            message: Some("update cancelled before activation".to_owned()),
        };
        operation.status = result.status.clone();
        operation.reason = result.reason.clone();
        operation.message = result.message.clone();
        operation.updated_at = now_rfc3339();
        write_operation(&operation)?;
        Ok(result)
    } else {
        Ok(OpResultBody {
            op_id: op_id.to_owned(),
            status: "rejected".to_owned(),
            reason: Some("update_already_started".to_owned()),
            message: Some(format!("update is already {}", operation.status)),
        })
    }
}

pub fn run_worker() -> Result<()> {
    loop {
        let mut operation = {
            let _lock = operation_lock()?;
            let entries = fs::read_dir(operations_dir()?)?;
            let mut candidate = None;
            for entry in entries {
                let path = entry?.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let bytes = match fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(?error, path = %path.display(), "skipping unreadable update state");
                        continue;
                    }
                };
                let operation: UpdateOperation = match serde_json::from_slice(&bytes) {
                    Ok(operation) => operation,
                    Err(error) => {
                        tracing::warn!(?error, path = %path.display(), "skipping invalid update state");
                        continue;
                    }
                };
                if operation.status == "accepted" {
                    candidate = Some(operation);
                    break;
                }
            }
            let mut operation = match candidate {
                Some(operation) => operation,
                None => return Ok(()),
            };
            operation.status = "running".to_owned();
            operation.updated_at = now_rfc3339();
            write_operation(&operation)?;
            operation
        };

        let target = &operation.target;
        let reference = flake_reference(&target.flake_ref, &target.revision);
        let ready_before = readiness_stamp();
        let profile = profile();
        if lease_expired(&operation.lease_expires_at) {
            let result = OpResultBody {
                op_id: operation.op_id.clone(),
                status: "failed".to_owned(),
                reason: Some("lease_expired".to_owned()),
                message: Some("update lease expired before realization".to_owned()),
            };
            set_result(&mut operation, &result)?;
            return Ok(());
        }
        let mut nix_args = vec![
            NIX_FEATURES[0],
            NIX_FEATURES[1],
            "profile",
            "install",
            "--profile",
            profile.as_str(),
        ];
        nix_args.push(&reference);
        let install = Command::new("nix")
            .env("PATH", nix_path())
            .args(nix_args)
            .output()
            .context("failed to execute nix profile install")?;
        if !install.status.success() {
            let result = OpResultBody {
                op_id: operation.op_id.clone(),
                status: "failed".to_owned(),
                reason: Some("realization_failed".to_owned()),
                message: Some(String::from_utf8_lossy(&install.stderr).trim().to_owned()),
            };
            set_result(&mut operation, &result)?;
            return Ok(());
        }

        if lease_expired(&operation.lease_expires_at) {
            let rollback = Command::new("nix")
                .env("PATH", nix_path())
                .args([
                    NIX_FEATURES[0],
                    NIX_FEATURES[1],
                    "profile",
                    "rollback",
                    "--profile",
                    profile.as_str(),
                ])
                .status()
                .context("failed to execute nix profile rollback")?;
            let result = OpResultBody {
                op_id: operation.op_id.clone(),
                status: "failed".to_owned(),
                reason: Some(
                    if rollback.success() {
                        "lease_expired"
                    } else {
                        "rollback_failed"
                    }
                    .to_owned(),
                ),
                message: Some(if rollback.success() {
                    "update lease expired before activation; new generation was rolled back"
                        .to_owned()
                } else {
                    "update lease expired and rollback could not be confirmed".to_owned()
                }),
            };
            set_result(&mut operation, &result)?;
            return Ok(());
        }

        let restart = Command::new("systemctl")
            .args(["restart", "statix-agent.service"])
            .status()
            .context("failed to restart statix-agent.service")?;
        let active = Command::new("systemctl")
            .args(["is-active", "--quiet", "statix-agent.service"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        let ready = wait_for_readiness(ready_before, Duration::from_secs(30));
        if !restart.success() || !active || !ready {
            let rollback = Command::new("nix")
                .env("PATH", nix_path())
                .args([
                    NIX_FEATURES[0],
                    NIX_FEATURES[1],
                    "profile",
                    "rollback",
                    "--profile",
                    profile.as_str(),
                ])
                .status()
                .context("failed to execute nix profile rollback")?;
            let restore = Command::new("systemctl")
                .args(["restart", "statix-agent.service"])
                .status()
                .context("failed to restart statix-agent.service after rollback")?;
            let result = OpResultBody {
                op_id: operation.op_id.clone(),
                status: "failed".to_owned(),
                reason: Some(
                    if !rollback.success() || !restore.success() {
                        "rollback_failed"
                    } else {
                        "health_check_failed"
                    }
                    .to_owned(),
                ),
                message: Some(if rollback.success() && restore.success() {
                    "new generation was rolled back".to_owned()
                } else {
                    "update failed and rollback could not be confirmed".to_owned()
                }),
            };
            set_result(&mut operation, &result)?;
            return Ok(());
        }

        let result = OpResultBody {
            op_id: operation.op_id.clone(),
            status: "succeeded".to_owned(),
            reason: None,
            message: Some(format!("activated revision {}", target.revision)),
        };
        set_result(&mut operation, &result)?;
    }
}

fn profile() -> String {
    std::env::var("STATIX_AGENT_PROFILE")
        .unwrap_or_else(|_| "/nix/var/nix/profiles/statix-agent".to_owned())
}

fn nix_path() -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    format!("/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin:{current}")
}

fn flake_reference(flake_ref: &str, revision: &str) -> String {
    let (without_fragment, fragment) = flake_ref
        .split_once('#')
        .map_or((flake_ref, None), |(base, fragment)| (base, Some(fragment)));
    let separator = if without_fragment.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut reference = format!("{without_fragment}{separator}rev={revision}#{}", PACKAGE);
    if let Some(fragment) = fragment.filter(|fragment| !fragment.is_empty()) {
        reference = format!("{without_fragment}{separator}rev={revision}#{fragment}");
    }
    reference
}

fn lease_expired(value: &str) -> bool {
    time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map(|expires| expires <= time::OffsetDateTime::now_utc())
        .unwrap_or(true)
}

fn readiness_stamp() -> Option<SystemTime> {
    fs::metadata(readiness_path())
        .and_then(|metadata| metadata.modified())
        .ok()
}
fn readiness_path() -> PathBuf {
    agent_state_dir()
        .map(|dir| dir.join("ready"))
        .unwrap_or_else(|_| PathBuf::from("/run/statix-agent-ready"))
}

pub fn mark_ready() -> Result<()> {
    let path = readiness_path();
    fs::write(&path, now_rfc3339())
        .with_context(|| format!("failed to write readiness marker {}", path.display()))
}

fn wait_for_readiness(previous: Option<SystemTime>, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(modified) =
            fs::metadata(readiness_path()).and_then(|metadata| metadata.modified())
        {
            if previous.map(|old| modified > old).unwrap_or(true) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_immutable_update_targets() {
        let result = accept(
            "op-1",
            UpdateAgentSpec {
                flake_ref: "github:example/agent".to_owned(),
                revision: String::new(),
            },
            "2099-01-01T00:00:00Z".to_owned(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn expired_leases_are_rejected() {
        assert!(lease_expired("2000-01-01T00:00:00Z"));
        assert!(lease_expired("not-a-timestamp"));
        assert!(!lease_expired("2099-01-01T00:00:00Z"));
    }
}
