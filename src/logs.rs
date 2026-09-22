//! Small, agent-owned log spool.  The control plane deliberately does not own
//! these records: it asks a connected agent for a bounded page when a user
//! opens a log view.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::agent_state_dir;

const MAX_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub id: String,
    pub timestamp: u64,
    pub scope: String,
    pub resource_id: Option<String>,
    pub job_id: Option<String>,
    pub attempt_id: Option<String>,
    pub stream: Option<String>,
    pub level: String,
    pub message: String,
}

fn directory() -> Result<PathBuf> {
    Ok(agent_state_dir()?.join("logs"))
}
fn path() -> Result<PathBuf> {
    // Daily files make the time retention bound real even on quiet nodes.
    Ok(directory()?.join(format!("events-{}.jsonl", now_ms() / 86_400_000)))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn redact(value: &str) -> String {
    // Do not try to guess every secret.  This catches the common values the
    // agent itself may emit while leaving normal command output useful.
    let mut value = value.to_owned();
    for marker in ["NODE_TOKEN=", "nodeToken=", "Authorization: Bearer "] {
        if let Some(start) = value.find(marker) {
            let end = value[start + marker.len()..]
                .find(char::is_whitespace)
                .map(|offset| start + marker.len() + offset)
                .unwrap_or(value.len());
            value.replace_range(start + marker.len()..end, "[REDACTED]");
        }
    }
    value
}

pub fn append_job(
    job_id: &str,
    attempt_id: &str,
    stream: &str,
    line: &str,
    scope: &str,
    resource_id: Option<&str>,
) -> Result<()> {
    fs::create_dir_all(directory()?)?;
    prune()?;
    let log_path = path()?;
    if log_path
        .metadata()
        .map(|meta| meta.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let rotated = directory()?.join(format!("events-overflow-{}.jsonl", now_ms()));
        fs::rename(&log_path, rotated)?;
        prune()?;
    }
    let record = Record {
        id: format!("{}-{}", now_ms(), uuid_suffix(line)),
        timestamp: now_ms(),
        scope: scope.into(),
        resource_id: resource_id.map(str::to_owned),
        job_id: Some(job_id.into()),
        attempt_id: Some(attempt_id.into()),
        stream: Some(stream.into()),
        level: if stream == "stderr" {
            "error".into()
        } else {
            "info".into()
        },
        message: redact(line),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    Ok(())
}

pub fn append_agent(level: &str, message: &str) -> Result<()> {
    fs::create_dir_all(directory()?)?;
    prune()?;
    let record = Record {
        id: format!("{}-{}", now_ms(), uuid_suffix(message)),
        timestamp: now_ms(),
        scope: "agent".into(),
        resource_id: None,
        job_id: None,
        attempt_id: None,
        stream: None,
        level: level.into(),
        message: redact(message),
    };
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path()?)?;
    serde_json::to_writer(&mut file, &record)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn uuid_suffix(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn prune() -> Result<()> {
    let cutoff = now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
    for entry in fs::read_dir(directory()?)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with("events-")
            && entry
                .metadata()?
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|t| (t.as_millis() as u64) < cutoff)
                .unwrap_or(false)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub fn query(
    scope: Option<&str>,
    resource_id: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<Record>, Option<String>)> {
    let mut records = Vec::new();
    let directory = directory()?;
    if !directory.exists() {
        return Ok((records, None));
    }
    let mut files = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|candidate| candidate.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
    files.sort();
    for log_path in files {
        for line in BufReader::new(fs::File::open(log_path)?).lines() {
            let record: Record = match serde_json::from_str(&line?) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if scope.is_some_and(|v| record.scope != v)
                || resource_id.is_some_and(|v| {
                    record.resource_id.as_deref() != Some(v) && record.job_id.as_deref() != Some(v)
                })
            {
                continue;
            }
            if cursor.is_some_and(|v| record.id.as_str() >= v) {
                continue;
            }
            records.push(record);
        }
    }
    records.reverse();
    records.truncate(limit.clamp(1, 500));
    let next = records.last().map(|r| r.id.clone());
    Ok((records, next))
}
