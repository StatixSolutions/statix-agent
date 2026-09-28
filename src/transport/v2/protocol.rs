//! Protocol v2 wire types: the envelope, session lifecycle, and the
//! desired/observed resource model. Ported field-for-field from
//! `apps/node-controller/node_modules/@statix/node-controller-contract/src/index.ts`
//! (the canonical source — that package is the implementation of
//! `plans/controlplane-agent-communicationsystem.md` §2-3). JSON field names
//! are camelCase to match it exactly.
//!
//! Only message types actually exchanged today are modeled. `op_dispatch`,
//! `op_cancel`, `op_result` and `logs.*` are not — node-controller's own
//! gateway doesn't send or act on them either yet (see
//! `AgentConnection.handleV2`'s default case, which just logs and no-ops).
//!
//! The desired-state model (`ServiceSpec`, `Healthcheck`, `RolloutSpec`, …)
//! and the status model's unused `Phase`/`ContainerState`/etc. variants are
//! deserialized/serialized in full for wire fidelity even though
//! `transport::v2::reconcile` only reads a handful of fields so far (it
//! doesn't execute anything yet, so most of this has nothing to act on)
//! hence the module-wide allow below rather than scattering it per field.

#![allow(dead_code)]

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub type Id = String;
pub type Generation = u64;
/// ISO-8601 UTC, e.g. "2026-09-28T12:00:00Z". Produced with [`now_rfc3339`]
/// for outgoing messages; carried opaquely (not parsed) for incoming ones.
pub type Timestamp = String;

pub const NODE_API_VERSION: &str = "statix.node/v1";
pub const CURRENT_PROTOCOL_VERSION: u8 = 2;

pub fn new_id() -> Id {
    ulid::Ulid::new().to_string()
}

pub fn now_rfc3339() -> Timestamp {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn unix_millis() -> i64 {
    let now = time::OffsetDateTime::now_utc();
    now.unix_timestamp() * 1000 + i64::from(now.millisecond())
}

// ─── envelope ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct OutgoingEnvelope<T> {
    v: u8,
    #[serde(rename = "type")]
    kind: &'static str,
    id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    ack: Option<Id>,
    ts: i64,
    body: T,
}

/// Encodes a `{v:2,type,id,ts,body}` frame, per node-controller-contract's
/// own `encodeEnvelope`.
pub fn encode<T: Serialize>(kind: &'static str, body: T) -> String {
    let envelope = OutgoingEnvelope {
        v: CURRENT_PROTOCOL_VERSION,
        kind,
        id: new_id(),
        ack: None,
        ts: unix_millis(),
        body,
    };
    serde_json::to_string(&envelope).expect("v2 envelope always serializes")
}

/// Loosely-shaped envelope, decoded first so `type` can pick the body
/// schema — the Rust analog of node-controller-contract's own two-step
/// `decodeV2Typed`.
#[derive(Debug, Deserialize)]
struct IncomingEnvelope {
    #[serde(rename = "type")]
    kind: String,
    body: serde_json::Value,
}

#[derive(Debug)]
pub enum DecodeError {
    InvalidEnvelope(serde_json::Error),
    InvalidBody {
        kind: String,
        error: serde_json::Error,
    },
}

/// Decoded, typed incoming message. Only the types this agent understands
/// are modeled; anything else surfaces as `Unhandled` rather than failing to
/// decode, so an unfamiliar (future) message type never kills the session.
#[derive(Debug)]
pub enum IncomingMessage {
    Challenge(ChallengeBody),
    Welcome(WelcomeBody),
    Goaway(GoawayBody),
    Ping,
    Desired(NodeDesiredState),
    Unhandled(String),
}

pub fn decode(raw: &str) -> Result<IncomingMessage, DecodeError> {
    let envelope: IncomingEnvelope =
        serde_json::from_str(raw).map_err(DecodeError::InvalidEnvelope)?;
    let kind = envelope.kind;
    let body_error = |error| DecodeError::InvalidBody {
        kind: kind.clone(),
        error,
    };
    match kind.as_str() {
        "challenge" => serde_json::from_value(envelope.body)
            .map(IncomingMessage::Challenge)
            .map_err(body_error),
        "welcome" => serde_json::from_value(envelope.body)
            .map(IncomingMessage::Welcome)
            .map_err(body_error),
        "goaway" => serde_json::from_value(envelope.body)
            .map(IncomingMessage::Goaway)
            .map_err(body_error),
        "ping" => Ok(IncomingMessage::Ping),
        "desired" => serde_json::from_value(envelope.body)
            .map(IncomingMessage::Desired)
            .map_err(body_error),
        other => Ok(IncomingMessage::Unhandled(other.to_string())),
    }
}

// ─── session lifecycle ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloBody {
    pub protocol: Vec<u8>,
    pub agent_version: String,
    pub boot_id: Id,
    pub capabilities: Vec<String>,
    /// objectId -> last generation the agent already has. Always empty for
    /// now: there is no local desired-state store yet, so every reconnect
    /// starts fresh (the server just re-sends the full snapshot, which is
    /// always valid per the "full snapshot, never a diff" design).
    pub observed: HashMap<Id, Generation>,
    pub pending_op_results: Vec<Id>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChallengeBody {
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthBody {
    pub node_id: Id,
    pub signature: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WelcomeBody {
    pub session_id: Id,
    pub heartbeat_sec: u32,
    pub resync_sec: u32,
    pub server_time: Timestamp,
    #[serde(default)]
    pub upgrade_required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoawayBody {
    pub retry_after_ms: u64,
}

// ─── resource model (desired state) ────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectMeta {
    pub id: Id,
    pub project_id: Id,
    pub node_id: Id,
    pub generation: Generation,
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuResources {
    pub count: u32,
    #[serde(default)]
    pub vendor: Option<GpuVendor>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    pub cpu: f64,
    pub memory_mb: u64,
    pub disk_gb: u64,
    #[serde(default)]
    pub gpu: Option<GpuResources>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeType {
    Lxc,
    Microvm,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpecBody {
    #[serde(rename = "type")]
    pub runtime_type: RuntimeType,
    pub image: String,
    pub resources: Resources,
    pub power: PowerState,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    pub api_version: String,
    pub kind: String,
    pub meta: ObjectMeta,
    pub spec: RuntimeSpecBody,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    pub id: Id,
    pub key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretMount {
    pub env: String,
    pub secret_ref: SecretRef,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortSpec {
    #[serde(default)]
    pub name: Option<String>,
    pub container: u16,
    pub protocol: PortProtocol,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExposeSpec {
    #[serde(default)]
    pub target_port: Option<u16>,
    pub protocol: PortProtocol,
    #[serde(default)]
    pub bridge_ip: Option<String>,
    #[serde(default)]
    pub bridge_port: Option<u16>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    OnFailure,
    Always,
    UnlessStopped,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Healthcheck {
    pub command: Vec<String>,
    pub interval_sec: u32,
    pub timeout_sec: u32,
    pub retries: u32,
    pub start_period_sec: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLimits {
    #[serde(default)]
    pub cpu: Option<f64>,
    #[serde(default)]
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    pub name: String,
    pub image: String,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub secrets: Vec<SecretMount>,
    #[serde(default)]
    pub mounts: Vec<VolumeMount>,
    #[serde(default)]
    pub ports: Vec<PortSpec>,
    #[serde(default)]
    pub expose: Option<ExposeSpec>,
    pub networks: Vec<String>,
    pub restart: RestartPolicy,
    #[serde(default)]
    pub healthcheck: Option<Healthcheck>,
    #[serde(default)]
    pub limits: Option<ServiceLimits>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSpec {
    #[serde(default)]
    pub internal: bool,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RolloutStrategy {
    Recreate,
    StartFirst,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RolloutSpec {
    pub strategy: RolloutStrategy,
    pub health_timeout_sec: u32,
    pub auto_rollback: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadSpecBody {
    pub runtime_id: Id,
    pub engine: String, // always "docker" today; room for others later
    pub definition_revision: u64,
    pub networks: HashMap<String, NetworkSpec>,
    pub services: Vec<ServiceSpec>,
    pub rollout: RolloutSpec,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadSpec {
    pub api_version: String,
    pub kind: String,
    pub meta: ObjectMeta,
    pub spec: WorkloadSpecBody,
}

/// Everything a node should be running. Objects absent from the lists are
/// torn down. Always a full snapshot, never a diff.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDesiredState {
    pub node_id: Id,
    pub generation: Generation,
    pub runtimes: Vec<RuntimeSpec>,
    pub workloads: Vec<WorkloadSpec>,
}

// ─── status: what the node actually observed ──────────────────────────────

#[derive(Debug, Clone, Copy, Serialize)]
pub enum Phase {
    Pending,
    Progressing,
    Ready,
    Degraded,
    Failed,
    Terminating,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ConditionStatus {
    True,
    False,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: ConditionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub last_transition_at: Timestamp,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerState {
    Created,
    Running,
    Restarting,
    Exited,
    Dead,
    Missing,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerHealth {
    Healthy,
    Unhealthy,
    Starting,
    None,
}

/// Not populated this pass (no real observation yet — see reconcile.rs), but
/// modeled so ObjectStatus's shape is complete and ready for it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    pub state: ContainerState,
    pub health: ContainerHealth,
    pub restart_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub oom_killed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    pub reason: String,
    pub message: String,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ObjectKind {
    Runtime,
    Workload,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectStatus {
    pub id: Id,
    pub kind: ObjectKind,
    pub node_id: Id,
    pub observed_generation: Generation,
    pub phase: Phase,
    pub conditions: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub services: Option<Vec<ServiceStatus>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<LastError>,
    pub reported_at: Timestamp,
}

// ─── events: the human-readable timeline ──────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeEvent {
    pub id: Id,
    pub node_id: Id,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<Generation>,
    pub severity: EventSeverity,
    pub reason: String,
    pub message: String,
    pub at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_hello_with_camel_case_fields() {
        let payload = encode(
            "hello",
            HelloBody {
                protocol: vec![2],
                agent_version: "1.0.0".to_string(),
                boot_id: "boot123".to_string(),
                capabilities: vec![],
                observed: HashMap::new(),
                pending_op_results: vec![],
            },
        );

        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["type"], "hello");
        assert_eq!(value["body"]["agentVersion"], "1.0.0");
        assert_eq!(value["body"]["bootId"], "boot123");
        assert_eq!(value["body"]["pendingOpResults"], serde_json::json!([]));
        assert!(value["id"].is_string());
        assert!(value["ts"].is_number());
    }

    #[test]
    fn decodes_challenge_and_welcome_and_goaway() {
        let challenge = decode(
            r#"{"v":2,"type":"challenge","id":"1","ts":0,"body":{"nonce":"0123456789abcdef"}}"#,
        )
        .unwrap();
        assert!(
            matches!(challenge, IncomingMessage::Challenge(body) if body.nonce == "0123456789abcdef")
        );

        let welcome = decode(
            r#"{"v":2,"type":"welcome","id":"1","ts":0,"body":{"sessionId":"s1","heartbeatSec":15,"resyncSec":300,"serverTime":"2026-01-01T00:00:00Z"}}"#,
        )
        .unwrap();
        assert!(
            matches!(welcome, IncomingMessage::Welcome(body) if body.session_id == "s1" && body.heartbeat_sec == 15)
        );

        let goaway =
            decode(r#"{"v":2,"type":"goaway","id":"1","ts":0,"body":{"retryAfterMs":5000}}"#)
                .unwrap();
        assert!(matches!(goaway, IncomingMessage::Goaway(body) if body.retry_after_ms == 5000));
    }

    #[test]
    fn decodes_ping_and_unhandled_types() {
        assert!(matches!(
            decode(r#"{"v":2,"type":"ping","id":"1","ts":0,"body":{}}"#).unwrap(),
            IncomingMessage::Ping
        ));
        assert!(matches!(
            decode(r#"{"v":2,"type":"op_dispatch","id":"1","ts":0,"body":{}}"#).unwrap(),
            IncomingMessage::Unhandled(kind) if kind == "op_dispatch"
        ));
    }

    #[test]
    fn decodes_desired_state_with_a_runtime_and_workload() {
        let raw = r#"{
            "v": 2, "type": "desired", "id": "1", "ts": 0,
            "body": {
                "nodeId": "n1",
                "generation": 3,
                "runtimes": [{
                    "apiVersion": "statix.node/v1", "kind": "Runtime",
                    "meta": {"id": "r1", "projectId": "p1", "nodeId": "n1", "generation": 1, "labels": {}},
                    "spec": {"type": "lxc", "image": "ubuntu:24.04", "resources": {"cpu": 2, "memoryMb": 4096, "diskGb": 20}, "power": "running"}
                }],
                "workloads": [{
                    "apiVersion": "statix.node/v1", "kind": "Workload",
                    "meta": {"id": "w1", "projectId": "p1", "nodeId": "n1", "generation": 1, "labels": {}},
                    "spec": {
                        "runtimeId": "r1", "engine": "docker", "definitionRevision": 7,
                        "networks": {"default": {"internal": false}},
                        "services": [{
                            "name": "api", "image": "nginx:1.27", "env": {}, "secrets": [], "mounts": [],
                            "ports": [], "networks": ["default"], "restart": "unless-stopped", "dependsOn": []
                        }],
                        "rollout": {"strategy": "recreate", "healthTimeoutSec": 120, "autoRollback": true}
                    }
                }]
            }
        }"#;

        let IncomingMessage::Desired(state) = decode(raw).unwrap() else {
            panic!("expected a Desired message");
        };
        assert_eq!(state.generation, 3);
        assert_eq!(state.runtimes.len(), 1);
        assert_eq!(state.workloads[0].spec.services[0].image, "nginx:1.27");
        assert!(matches!(
            state.runtimes[0].spec.runtime_type,
            RuntimeType::Lxc
        ));
    }

    #[test]
    fn encodes_status_with_condition_status_pascal_case() {
        let status = ObjectStatus {
            id: "r1".to_string(),
            kind: ObjectKind::Runtime,
            node_id: "n1".to_string(),
            observed_generation: 1,
            phase: Phase::Pending,
            conditions: vec![Condition {
                condition_type: "Available".to_string(),
                status: ConditionStatus::False,
                reason: Some("Unsupported".to_string()),
                message: None,
                last_transition_at: "2026-01-01T00:00:00Z".to_string(),
            }],
            services: None,
            last_error: None,
            reported_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["phase"], "Pending");
        assert_eq!(value["conditions"][0]["status"], "False");
        assert_eq!(value["observedGeneration"], 1);
    }
}
