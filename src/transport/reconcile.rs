//! The "verify current against target" step: given a [`NodeDesiredState`],
//! produce the [`ObjectStatus`] reports and a human-readable log block. Pure
//! and side-effect free — no docker/lxc calls.
//!
//! Real execution (and therefore real observation of what's actually
//! running) is still disconnected: job execution is not wired up
//! yet. So every object here is reported honestly as
//! `Pending` with an `Unsupported` condition, rather than claiming anything
//! ran — the protocol-correct way to say "I understood this spec, I'm just
//! not acting on it yet" (see `Reason::Unsupported` in
//! node-controller-contract: "agent lacks the capability for this spec").

use super::protocol::{
    Condition, ConditionStatus, EventSeverity, NodeDesiredState, NodeEvent, ObjectKind,
    ObjectStatus, Phase, RuntimeSpec, WorkloadSpec, new_id, now_rfc3339,
};

const UNSUPPORTED_MESSAGE: &str = "execution not yet implemented in this build";

pub struct ReconcilePlan {
    pub statuses: Vec<ObjectStatus>,
    pub event: NodeEvent,
    /// Multi-line, indented summary of every desired object — same
    /// Display-block idea as `transport::intent::JobIntent`.
    pub block: String,
}

pub fn plan(state: &NodeDesiredState) -> ReconcilePlan {
    let mut statuses = Vec::with_capacity(state.runtimes.len() + state.workloads.len());
    let mut lines = Vec::new();

    for runtime in &state.runtimes {
        statuses.push(pending_status(
            ObjectKind::Runtime,
            &runtime.meta.id,
            &runtime.meta.node_id,
            runtime.meta.generation,
        ));
        lines.push(describe_runtime(runtime));
    }
    for workload in &state.workloads {
        statuses.push(pending_status(
            ObjectKind::Workload,
            &workload.meta.id,
            &workload.meta.node_id,
            workload.meta.generation,
        ));
        lines.push(describe_workload(workload));
    }

    let block = if lines.is_empty() {
        "  (no runtimes or workloads desired)".to_string()
    } else {
        lines.join("\n")
    };

    let event = NodeEvent {
        id: new_id(),
        node_id: state.node_id.clone(),
        project_id: None,
        object_id: None,
        generation: Some(state.generation),
        severity: EventSeverity::Info,
        reason: "DesiredStateReceived".to_string(),
        message: format!(
            "generation {}: {} runtime(s), {} workload(s), none executed (not yet implemented)",
            state.generation,
            state.runtimes.len(),
            state.workloads.len()
        ),
        at: now_rfc3339(),
    };

    ReconcilePlan {
        statuses,
        event,
        block,
    }
}

fn pending_status(kind: ObjectKind, id: &str, node_id: &str, generation: u64) -> ObjectStatus {
    ObjectStatus {
        id: id.to_string(),
        kind,
        node_id: node_id.to_string(),
        observed_generation: generation,
        phase: Phase::Pending,
        conditions: vec![Condition {
            condition_type: "Available".to_string(),
            status: ConditionStatus::False,
            reason: Some("Unsupported".to_string()),
            message: Some(UNSUPPORTED_MESSAGE.to_string()),
            last_transition_at: now_rfc3339(),
        }],
        services: None,
        last_error: None,
        reported_at: now_rfc3339(),
    }
}

fn describe_runtime(runtime: &RuntimeSpec) -> String {
    format!(
        "  runtime {} (project {}, generation {}): type={:?} image={} power={:?}",
        runtime.meta.id,
        runtime.meta.project_id,
        runtime.meta.generation,
        runtime.spec.runtime_type,
        runtime.spec.image,
        runtime.spec.power,
    )
}

fn describe_workload(workload: &WorkloadSpec) -> String {
    let services = workload
        .spec
        .services
        .iter()
        .map(|service| format!("{}={}", service.name, service.image))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "  workload {} (project {}, runtime {}, generation {}): {} service(s): {}",
        workload.meta.id,
        workload.meta.project_id,
        workload.spec.runtime_id,
        workload.meta.generation,
        workload.spec.services.len(),
        services,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::protocol::{
        NetworkSpec, ObjectMeta, PowerState, Resources, RestartPolicy, RolloutSpec,
        RolloutStrategy, RuntimeSpecBody, RuntimeType, ServiceSpec, WorkloadSpecBody,
    };
    use std::collections::HashMap;

    fn runtime(id: &str, generation: u64) -> RuntimeSpec {
        RuntimeSpec {
            api_version: "statix.node/v1".to_string(),
            kind: "Runtime".to_string(),
            meta: ObjectMeta {
                id: id.to_string(),
                project_id: "p1".to_string(),
                node_id: "n1".to_string(),
                generation,
                labels: HashMap::new(),
            },
            spec: RuntimeSpecBody {
                runtime_type: RuntimeType::Lxc,
                image: "ubuntu:24.04".to_string(),
                resources: Resources {
                    cpu: 2.0,
                    memory_mb: 4096,
                    disk_gb: 20,
                    gpu: None,
                },
                power: PowerState::Running,
            },
        }
    }

    fn workload(id: &str, runtime_id: &str, services: Vec<&str>) -> WorkloadSpec {
        WorkloadSpec {
            api_version: "statix.node/v1".to_string(),
            kind: "Workload".to_string(),
            meta: ObjectMeta {
                id: id.to_string(),
                project_id: "p1".to_string(),
                node_id: "n1".to_string(),
                generation: 1,
                labels: HashMap::new(),
            },
            spec: WorkloadSpecBody {
                runtime_id: runtime_id.to_string(),
                engine: "docker".to_string(),
                definition_revision: 7,
                networks: HashMap::from([("default".to_string(), NetworkSpec { internal: false })]),
                services: services
                    .into_iter()
                    .map(|name| ServiceSpec {
                        name: name.to_string(),
                        image: format!("{name}:latest"),
                        command: None,
                        entrypoint: None,
                        env: HashMap::new(),
                        secrets: Vec::new(),
                        mounts: Vec::new(),
                        ports: Vec::new(),
                        expose: None,
                        networks: vec!["default".to_string()],
                        restart: RestartPolicy::UnlessStopped,
                        healthcheck: None,
                        limits: None,
                        depends_on: Vec::new(),
                    })
                    .collect(),
                rollout: RolloutSpec {
                    strategy: RolloutStrategy::Recreate,
                    health_timeout_sec: 120,
                    auto_rollback: true,
                },
            },
        }
    }

    #[test]
    fn plans_one_status_per_object_all_pending_and_unsupported() {
        let state = NodeDesiredState {
            node_id: "n1".to_string(),
            generation: 3,
            runtimes: vec![runtime("r1", 1)],
            workloads: vec![workload("w1", "r1", vec!["api", "worker"])],
        };

        let plan = plan(&state);

        assert_eq!(plan.statuses.len(), 2);
        for status in &plan.statuses {
            assert!(matches!(status.phase, Phase::Pending));
            assert_eq!(status.conditions.len(), 1);
            assert_eq!(status.conditions[0].reason.as_deref(), Some("Unsupported"));
        }
        assert_eq!(plan.event.generation, Some(3));
        assert!(plan.event.message.contains("1 runtime(s), 1 workload(s)"));
    }

    #[test]
    fn block_lists_runtime_and_workload_services() {
        let state = NodeDesiredState {
            node_id: "n1".to_string(),
            generation: 1,
            runtimes: vec![runtime("r1", 1)],
            workloads: vec![workload("w1", "r1", vec!["api"])],
        };

        let block = plan(&state).block;

        assert!(block.contains("runtime r1"));
        assert!(block.contains("workload w1"));
        assert!(block.contains("api=api:latest"));
        assert!(block.lines().all(|line| line.starts_with("  ")));
    }

    #[test]
    fn empty_desired_state_produces_no_statuses_and_a_placeholder_block() {
        let state = NodeDesiredState {
            node_id: "n1".to_string(),
            generation: 0,
            runtimes: Vec::new(),
            workloads: Vec::new(),
        };

        let plan = plan(&state);

        assert!(plan.statuses.is_empty());
        assert_eq!(plan.block, "  (no runtimes or workloads desired)");
    }
}
