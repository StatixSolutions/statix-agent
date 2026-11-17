//! Durable orchestration-issued agent updates.
//!
//! The agent owns only the execution of an explicitly addressed update. The
//! orchestration plane owns rollout timing, targeting, and retry policy.

use std::{fs, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;

use crate::config::agent_state_dir;
use crate::transport::protocol::{Id, OpResultBody, UpdateAgentSpec, now_rfc3339};

const PROFILE: &str = "/nix/var/nix/profiles/statix-agent";
const PACKAGE: &str = "statix-agent-runtime";

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
    Ok(operations_dir()?.join(format!("{op_id}.json")))
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
    Ok(Some(serde_json::from_slice(&bytes).context("invalid update operation state")?))
}

pub fn pending_results() -> Result<Vec<OpResultBody>> {
    let directory = operations_dir()?;
    let mut results = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let operation: UpdateOperation = serde_json::from_slice(&bytes)?;
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
    let mut operation = load_operation(op_id)?.context("unknown update operation")?;
    if operation.status == "accepted" {
        let result = OpResultBody {
            op_id: op_id.to_owned(),
            status: "rejected".to_owned(),
            reason: Some("cancelled".to_owned()),
            message: Some("update cancelled before activation".to_owned()),
        };
        set_result(&mut operation, &result)?;
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
    let entries = fs::read_dir(operations_dir()?)?;
    let mut candidate = None;
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let operation: UpdateOperation = serde_json::from_slice(&fs::read(path)?)?;
        if operation.status == "accepted" {
            candidate = Some(operation);
            break;
        }
    }
    let mut operation = candidate.context("no accepted update operation")?;
    operation.status = "running".to_owned();
    operation.updated_at = now_rfc3339();
    write_operation(&operation)?;

    let target = &operation.target;
    let reference = format!("{}?rev={}#{}", target.flake_ref, target.revision, PACKAGE);
    let install = Command::new("nix")
        .args(["profile", "install", "--profile", PROFILE, &reference])
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

    let restart = Command::new("systemctl")
        .args(["restart", "statix-agent.service"])
        .status()
        .context("failed to restart statix-agent.service")?;
    let active = Command::new("systemctl")
        .args(["is-active", "--quiet", "statix-agent.service"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !restart.success() || !active {
        let _ = Command::new("nix")
            .args(["profile", "rollback", "--profile", PROFILE])
            .status();
        let _ = Command::new("systemctl")
            .args(["restart", "statix-agent.service"])
            .status();
        let result = OpResultBody {
            op_id: operation.op_id.clone(),
            status: "failed".to_owned(),
            reason: Some("health_check_failed".to_owned()),
            message: Some("new generation was rolled back".to_owned()),
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
    set_result(&mut operation, &result)
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
}
