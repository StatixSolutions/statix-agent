//! Describes what an [`AgentJob`](super::protocol::AgentJob) would do, purely
//! from its opaque JSON `spec`, without executing anything. This is the
//! "what it would do" half of `--debug-log-only`: it runs unconditionally,
//! flag or no flag, so the same clear description is logged either way.
//!
//! Deliberately reads fields straight off `serde_json::Value` rather than
//! deserializing into a typed per-kind enum: the only consumer is logging,
//! and a full typed model (with every optional execution field the old
//! runner needed) would just be dead weight now that nothing executes.

use std::fmt;

use serde_json::Value;

pub struct JobIntent {
    pub kind: String,
    /// One-line human-readable summary, safe to log at `info` level.
    pub summary: String,
    /// Extra structured key/value pairs worth attaching to the log event.
    pub fields: Vec<(&'static str, String)>,
}

/// Renders as an indented multi-line block (kind, then each field, then the
/// summary sentence last) rather than a single crammed line — this is what
/// makes `transport::dispatch`'s "received job" log actually readable.
impl fmt::Display for JobIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "  kind: {}", self.kind)?;
        for (key, value) in &self.fields {
            writeln!(f, "  {key}: {value}")?;
        }
        write!(f, "  summary: {}", self.summary)
    }
}

pub fn describe(spec: &Value) -> JobIntent {
    let kind = spec
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    match kind {
        "deploy_docker" => describe_deploy_docker(spec),
        "create_runtime" => describe_create_runtime(spec),
        "start_runtime" => describe_runtime_lifecycle(spec, "start"),
        "stop_runtime" => describe_runtime_lifecycle(spec, "stop"),
        "destroy_runtime" => describe_runtime_lifecycle(spec, "destroy"),
        "deploy_bundle" => describe_deploy_bundle(spec),
        "run_test" => describe_run_test(spec),
        "update_agent" => describe_update_agent(spec),
        other => describe_unknown(other, spec),
    }
}

fn str_field<'a>(spec: &'a Value, key: &str) -> &'a str {
    spec.get(key).and_then(Value::as_str).unwrap_or("?")
}

fn describe_deploy_docker(spec: &Value) -> JobIntent {
    let project_id = str_field(spec, "projectId");
    let runtime_id = str_field(spec, "runtimeId");
    let revision = spec.get("revision").and_then(Value::as_u64);
    let networks = spec
        .get("networks")
        .and_then(Value::as_object)
        .map(|networks| networks.keys().cloned().collect::<Vec<_>>().join(", "))
        .unwrap_or_default();

    let services = spec.get("services").and_then(Value::as_object);
    let service_count = services.map(|services| services.len()).unwrap_or(0);
    let service_summaries: Vec<String> = services
        .map(|services| {
            services
                .iter()
                .map(|(name, service)| {
                    let image = service.get("image").and_then(Value::as_str).unwrap_or("?");
                    let exposed = service
                        .get("expose")
                        .and_then(Value::as_object)
                        .map(|_| " (exposed)")
                        .unwrap_or("");
                    format!("{name}={image}{exposed}")
                })
                .collect()
        })
        .unwrap_or_default();

    JobIntent {
        kind: "deploy_docker".to_string(),
        summary: format!(
            "deploy {service_count} service(s) to runtime {runtime_id} (project {project_id}, revision {revision}): {services}",
            revision = revision
                .map(|r| r.to_string())
                .unwrap_or_else(|| "?".to_string()),
            services = service_summaries.join(", "),
        ),
        fields: vec![
            ("project_id", project_id.to_string()),
            ("runtime_id", runtime_id.to_string()),
            (
                "revision",
                revision
                    .map(|r| r.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            ),
            ("networks", networks),
            ("service_count", service_count.to_string()),
        ],
    }
}

fn describe_create_runtime(spec: &Value) -> JobIntent {
    let project_id = str_field(spec, "projectId");
    let runtime_id = str_field(spec, "runtimeId");
    let image = spec
        .get("image")
        .and_then(Value::as_str)
        .unwrap_or("ubuntu:24.04");
    let cpu = spec.get("cpu").and_then(Value::as_u64);
    let memory_mb = spec.get("memoryMb").and_then(Value::as_u64);

    JobIntent {
        kind: "create_runtime".to_string(),
        summary: format!(
            "create runtime {runtime_id} (project {project_id}) image={image} cpu={cpu} memory_mb={memory_mb}",
            cpu = cpu
                .map(|v| v.to_string())
                .unwrap_or_else(|| "default".to_string()),
            memory_mb = memory_mb
                .map(|v| v.to_string())
                .unwrap_or_else(|| "default".to_string()),
        ),
        fields: vec![
            ("project_id", project_id.to_string()),
            ("runtime_id", runtime_id.to_string()),
            ("image", image.to_string()),
        ],
    }
}

fn describe_runtime_lifecycle(spec: &Value, action: &str) -> JobIntent {
    let project_id = str_field(spec, "projectId");
    let runtime_id = str_field(spec, "runtimeId");

    JobIntent {
        kind: format!("{action}_runtime"),
        summary: format!("{action} runtime {runtime_id} (project {project_id})"),
        fields: vec![
            ("project_id", project_id.to_string()),
            ("runtime_id", runtime_id.to_string()),
        ],
    }
}

fn describe_deploy_bundle(spec: &Value) -> JobIntent {
    let deployment_id = str_field(spec, "deploymentId");
    let project_id = str_field(spec, "projectId");
    let environment = str_field(spec, "environment");
    let dry_run = spec.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
    let bundle = spec.get("bundle");
    let size_bytes = bundle
        .and_then(|bundle| bundle.get("sizeBytes"))
        .and_then(Value::as_u64);
    let sha256 = bundle
        .and_then(|bundle| bundle.get("sha256"))
        .and_then(Value::as_str)
        .unwrap_or("?");

    JobIntent {
        kind: "deploy_bundle".to_string(),
        summary: format!(
            "deploy bundle to project {project_id} env {environment} (deployment {deployment_id}){dry_run_suffix}",
            dry_run_suffix = if dry_run { " [dry run]" } else { "" },
        ),
        fields: vec![
            ("deployment_id", deployment_id.to_string()),
            ("project_id", project_id.to_string()),
            ("environment", environment.to_string()),
            ("dry_run", dry_run.to_string()),
            (
                "bundle_size_bytes",
                size_bytes
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "?".to_string()),
            ),
            ("bundle_sha256", sha256.to_string()),
        ],
    }
}

fn describe_run_test(spec: &Value) -> JobIntent {
    let preset = str_field(spec, "preset");
    let source = spec.get("source");
    let git_ref = source
        .and_then(|source| source.get("ref"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let commit_sha = source
        .and_then(|source| source.get("commitSha"))
        .and_then(Value::as_str)
        .unwrap_or("?");
    let timeout_seconds = spec.get("timeoutSeconds").and_then(Value::as_u64);

    JobIntent {
        kind: "run_test".to_string(),
        summary: format!("run test preset={preset} ref={git_ref}@{commit_sha}"),
        fields: vec![
            ("preset", preset.to_string()),
            ("git_ref", git_ref.to_string()),
            ("commit_sha", commit_sha.to_string()),
            (
                "timeout_seconds",
                timeout_seconds
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "default".to_string()),
            ),
        ],
    }
}

fn describe_update_agent(spec: &Value) -> JobIntent {
    let target_version = spec
        .get("targetVersion")
        .and_then(Value::as_str)
        .unwrap_or("latest");
    let channel = spec
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or("default");

    JobIntent {
        kind: "update_agent".to_string(),
        summary: format!("update agent to version={target_version} channel={channel}"),
        fields: vec![
            ("target_version", target_version.to_string()),
            ("channel", channel.to_string()),
        ],
    }
}

fn describe_unknown(kind: &str, spec: &Value) -> JobIntent {
    JobIntent {
        kind: kind.to_string(),
        summary: format!("unknown job kind '{kind}'; spec={spec}"),
        fields: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn displays_as_an_indented_multi_line_block() {
        let intent = describe(&json!({
            "kind": "create_runtime",
            "projectId": "p1",
            "runtimeId": "r1",
        }));

        let rendered = intent.to_string();
        let lines: Vec<&str> = rendered.lines().collect();

        assert_eq!(lines[0], "  kind: create_runtime");
        assert!(lines[1..].iter().all(|line| line.starts_with("  ")));
        assert_eq!(
            lines.last().unwrap(),
            &"  summary: create runtime r1 (project p1) image=ubuntu:24.04 cpu=default memory_mb=default"
        );
    }

    #[test]
    fn describes_deploy_docker_services_and_exposure() {
        let intent = describe(&json!({
            "kind": "deploy_docker",
            "projectId": "p1",
            "runtimeId": "r1",
            "revision": 7,
            "networks": {"default": {}},
            "services": {
                "api": {"image": "nginx:1.27", "expose": {"protocol": "tcp"}},
                "worker": {"image": "worker:latest"},
            },
        }));

        assert_eq!(intent.kind, "deploy_docker");
        assert!(intent.summary.contains("2 service(s)"));
        assert!(intent.summary.contains("runtime r1"));
        assert!(intent.summary.contains("project p1"));
        assert!(intent.summary.contains("revision 7"));
        assert!(intent.summary.contains("api=nginx:1.27 (exposed)"));
        assert!(intent.summary.contains("worker=worker:latest"));
        assert!(intent.fields.contains(&("service_count", "2".to_string())));
    }

    #[test]
    fn describes_create_runtime_defaults() {
        let intent = describe(&json!({
            "kind": "create_runtime",
            "projectId": "p1",
            "runtimeId": "r1",
        }));

        assert_eq!(intent.kind, "create_runtime");
        assert!(intent.summary.contains("image=ubuntu:24.04"));
        assert!(intent.summary.contains("cpu=default"));
    }

    #[test]
    fn describes_runtime_lifecycle_actions() {
        for (kind, action) in [
            ("start_runtime", "start"),
            ("stop_runtime", "stop"),
            ("destroy_runtime", "destroy"),
        ] {
            let intent = describe(&json!({"kind": kind, "projectId": "p1", "runtimeId": "r1"}));
            assert_eq!(intent.summary, format!("{action} runtime r1 (project p1)"));
        }
    }

    #[test]
    fn describes_deploy_bundle_dry_run() {
        let intent = describe(&json!({
            "kind": "deploy_bundle",
            "deploymentId": "d1",
            "projectId": "p1",
            "environment": "prod",
            "dryRun": true,
            "bundle": {"sizeBytes": 1024, "sha256": "abc"},
        }));

        assert!(intent.summary.contains("[dry run]"));
        assert!(
            intent
                .fields
                .contains(&("bundle_sha256", "abc".to_string()))
        );
    }

    #[test]
    fn describes_run_test_source() {
        let intent = describe(&json!({
            "kind": "run_test",
            "preset": "cargo_test",
            "source": {"ref": "main", "commitSha": "deadbeef"},
        }));

        assert_eq!(
            intent.summary,
            "run test preset=cargo_test ref=main@deadbeef"
        );
    }

    #[test]
    fn describes_update_agent_target() {
        let intent = describe(&json!({
            "kind": "update_agent",
            "targetVersion": "1.2.3",
            "channel": "stable",
        }));

        assert_eq!(
            intent.summary,
            "update agent to version=1.2.3 channel=stable"
        );
    }

    #[test]
    fn falls_back_to_raw_spec_for_unknown_kind() {
        let intent = describe(&json!({"kind": "mystery", "foo": "bar"}));

        assert_eq!(intent.kind, "mystery");
        assert!(intent.summary.contains("unknown job kind 'mystery'"));
        assert!(intent.summary.contains("\"foo\":\"bar\""));
    }
}
