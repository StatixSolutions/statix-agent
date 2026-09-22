//! Small, agent-owned log spool.  The control plane deliberately does not own
//! these records: it asks a connected agent for a bounded page when a user
//! opens a log view.
use std::{
    cmp::Ordering,
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::agent_state_dir;

const MAX_BYTES: u64 = 100 * 1024 * 1024;
const REDACTED: &str = "[REDACTED]";

// All spool operations in the agent process share this lock. In addition to
// making rotation deterministic, it prevents separate tracing and job-output
// writers from interleaving fragments of JSON records.
static SPOOL_LOCK: Mutex<()> = Mutex::new(());
static LAST_RECORD_ID: AtomicU64 = AtomicU64::new(0);

/// `tracing-subscriber` writer which preserves the systemd/stderr output and
/// mirrors every completed formatted line into the agent-owned spool.
pub struct AgentLogWriter {
    stderr: io::Stderr,
    pending: Vec<u8>,
}

pub fn agent_log_writer() -> AgentLogWriter {
    AgentLogWriter {
        stderr: io::stderr(),
        pending: Vec::new(),
    }
}

impl Write for AgentLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stderr.write_all(bytes)?;
        self.pending.extend_from_slice(bytes);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = String::from_utf8_lossy(&self.pending[..newline])
                .trim()
                .to_owned();
            self.pending.drain(..=newline);
            if !line.is_empty() {
                let level = if line.contains(" ERROR ") {
                    "error"
                } else if line.contains(" WARN ") {
                    "warn"
                } else if line.contains(" DEBUG ") {
                    "debug"
                } else {
                    "info"
                };
                let _ = append_agent(level, &line);
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()
    }
}

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
        let mut offset = 0;
        while let Some(relative_start) = value[offset..].find(marker) {
            let start = offset + relative_start;
            let secret_start = start + marker.len();
            let end = value[secret_start..]
                .find(char::is_whitespace)
                .map(|secret_len| secret_start + secret_len)
                .unwrap_or(value.len());
            value.replace_range(secret_start..end, REDACTED);
            offset = secret_start + REDACTED.len();
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
    let timestamp = now_ms();
    let record = Record {
        id: next_record_id(),
        timestamp,
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
    append(record)
}

pub fn append_agent(level: &str, message: &str) -> Result<()> {
    let timestamp = now_ms();
    let record = Record {
        id: next_record_id(),
        timestamp,
        scope: "agent".into(),
        resource_id: None,
        job_id: None,
        attempt_id: None,
        stream: None,
        level: level.into(),
        message: redact(message),
    };
    append(record)
}

fn append(record: Record) -> Result<()> {
    let _guard = SPOOL_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("agent log spool lock is poisoned"))?;
    append_to(&directory()?, record)
}

fn append_to(directory: &Path, record: Record) -> Result<()> {
    append_to_with_limit(directory, record, MAX_BYTES)
}

fn append_to_with_limit(directory: &Path, record: Record, max_bytes: u64) -> Result<()> {
    fs::create_dir_all(directory)?;
    prune(directory)?;

    // Serialize before opening the file so every record is appended with one
    // write while the process-wide spool lock is held.
    let mut payload = serde_json::to_vec(&record)?;
    payload.push(b'\n');

    let log_path = path_in(directory);
    let current_len = log_path.metadata().map(|meta| meta.len()).unwrap_or(0);
    if current_len > 0 && current_len.saturating_add(payload.len() as u64) > max_bytes {
        let rotated = directory.join(format!("events-overflow-{}.jsonl", next_record_id()));
        fs::rename(&log_path, rotated)?;
        prune(directory)?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    file.write_all(&payload)?;
    Ok(())
}

fn path_in(directory: &Path) -> PathBuf {
    // Daily files make the time retention bound real even on quiet nodes.
    directory.join(format!("events-{}.jsonl", now_ms() / 86_400_000))
}

fn next_record_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64;
    let id = LAST_RECORD_ID
        .fetch_update(
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
            |previous| Some(now.max(previous.saturating_add(1))),
        )
        .unwrap_or_else(|previous| previous.saturating_add(1));
    format!("{id:020}-{:010}", std::process::id())
}

fn prune(directory: &Path) -> Result<()> {
    let cutoff = now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
    for entry in fs::read_dir(directory)? {
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
    let _guard = SPOOL_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("agent log spool lock is poisoned"))?;
    query_in(&directory()?, scope, resource_id, cursor, limit)
}

fn query_in(
    directory: &Path,
    scope: Option<&str>,
    resource_id: Option<&str>,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(Vec<Record>, Option<String>)> {
    let mut records = Vec::new();
    if !directory.exists() {
        return Ok((records, None));
    }
    let files = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|candidate| candidate.extension().is_some_and(|ext| ext == "jsonl"))
        .collect::<Vec<_>>();
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
            records.push(record);
        }
    }
    records.sort_unstable_by(compare_records_newest_first);

    if let Some(cursor) = cursor {
        if let Some((timestamp, id)) = decode_cursor(cursor) {
            records.retain(|record| {
                record.timestamp < timestamp
                    || (record.timestamp == timestamp && record.id.as_str() < id)
            });
        } else if let Some(position) = records.iter().position(|record| record.id == cursor) {
            // Accept cursors emitted by older agents during rolling upgrades.
            records.drain(..=position);
        }
    }

    let limit = limit.clamp(1, 500);
    let has_more = records.len() > limit;
    records.truncate(limit);
    let next = has_more
        .then(|| records.last().map(encode_cursor))
        .flatten();
    Ok((records, next))
}

fn compare_records_newest_first(left: &Record, right: &Record) -> Ordering {
    right
        .timestamp
        .cmp(&left.timestamp)
        .then_with(|| right.id.cmp(&left.id))
}

fn encode_cursor(record: &Record) -> String {
    format!("{}|{}", record.timestamp, record.id)
}

fn decode_cursor(cursor: &str) -> Option<(u64, &str)> {
    let (timestamp, id) = cursor.split_once('|')?;
    Some((timestamp.parse().ok()?, id))
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Arc, thread};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("statix-agent-logs-{}", next_record_id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(id: &str, timestamp: u64, scope: &str, message: &str) -> Record {
        Record {
            id: id.to_string(),
            timestamp,
            scope: scope.to_string(),
            resource_id: None,
            job_id: None,
            attempt_id: None,
            stream: None,
            level: "info".to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn redacts_every_occurrence_of_each_secret_marker() {
        let input = "NODE_TOKEN=one nodeToken=two NODE_TOKEN=three Authorization: Bearer four Authorization: Bearer five";

        let redacted = redact(input);

        assert_eq!(redacted.matches(REDACTED).count(), 5);
        for secret in ["one", "two", "three", "four", "five"] {
            assert!(!redacted.contains(secret));
        }
    }

    #[test]
    fn generated_record_ids_are_unique_across_concurrent_writers() {
        let ids = Arc::new(Mutex::new(Vec::new()));
        let threads = (0..8)
            .map(|_| {
                let ids = Arc::clone(&ids);
                thread::spawn(move || {
                    let generated = (0..100).map(|_| next_record_id()).collect::<Vec<_>>();
                    ids.lock().unwrap().extend(generated);
                })
            })
            .collect::<Vec<_>>();
        for handle in threads {
            handle.join().unwrap();
        }

        let ids = ids.lock().unwrap();
        assert_eq!(ids.len(), ids.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn pagination_uses_explicit_record_order_without_omissions() {
        let directory = TestDirectory::new();
        for value in [
            record("same-b", 20, "job", "second"),
            record("older", 10, "job", "oldest"),
            record("same-c", 20, "job", "first"),
            record("same-a", 20, "job", "third"),
        ] {
            append_to(&directory.0, value).unwrap();
        }

        let (first, cursor) = query_in(&directory.0, Some("job"), None, None, 2).unwrap();
        let (second, final_cursor) =
            query_in(&directory.0, Some("job"), None, cursor.as_deref(), 2).unwrap();

        assert_eq!(
            first
                .iter()
                .map(|value| value.id.as_str())
                .collect::<Vec<_>>(),
            ["same-c", "same-b"]
        );
        assert_eq!(
            second
                .iter()
                .map(|value| value.id.as_str())
                .collect::<Vec<_>>(),
            ["same-a", "older"]
        );
        assert!(final_cursor.is_none());
    }

    #[test]
    fn concurrent_appends_produce_complete_json_records() {
        let directory = Arc::new(TestDirectory::new());
        let threads = (0..4)
            .map(|thread_index| {
                let directory = Arc::clone(&directory);
                thread::spawn(move || {
                    for record_index in 0..25 {
                        let _guard = SPOOL_LOCK.lock().unwrap();
                        append_to(
                            &directory.0,
                            record(
                                &next_record_id(),
                                record_index,
                                "job",
                                &format!("thread-{thread_index}-record-{record_index}"),
                            ),
                        )
                        .unwrap();
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in threads {
            handle.join().unwrap();
        }

        let (records, _) = query_in(&directory.0, None, None, None, 500).unwrap();
        assert_eq!(records.len(), 100);
    }

    #[test]
    fn diagnostic_records_use_the_same_rotation_limit() {
        let directory = TestDirectory::new();
        let first = record("agent-a", 1, "agent", &"a".repeat(300));
        let second = record("agent-b", 2, "agent", &"b".repeat(300));
        let single_record_size = serde_json::to_vec(&first).unwrap().len() as u64 + 1;
        let max_bytes = single_record_size + 1;

        append_to_with_limit(&directory.0, first, max_bytes).unwrap();
        append_to_with_limit(&directory.0, second, max_bytes).unwrap();

        let files = fs::read_dir(&directory.0)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2);
        assert!(
            files
                .iter()
                .all(|entry| entry.metadata().unwrap().len() <= max_bytes)
        );
    }
}
