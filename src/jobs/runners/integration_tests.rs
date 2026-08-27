use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::jobs::{
    CommandSpec, ExecutionContext, PreparedWorkspace, RunnerEnvironment, execute_spec,
};

fn test_root(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "statix-runner-{name}-{}-{stamp}",
        std::process::id()
    ))
}

const TEST_IMAGE: &str = "ghcr.io/statixsolutions/statix-agent/testing-images/8080-ok:latest";

fn compose_command() -> CommandSpec {
    CommandSpec {
        argv: vec![
            "bash".to_string(),
            "-lc".to_string(),
            "set -eu; test \"$(id -un)\" = statix; test -d /home/statix/docker; test \"$(stat -c %U /home/statix/docker)\" = statix; docker version; docker compose version; docker compose up -d; success=; for attempt in $(seq 1 30); do response=$(curl -fsS http://127.0.0.1:8080 || true); if [ \"$response\" = \"ok: success\" ]; then printf '%s' \"$response\"; success=true; break; fi; sleep 1; done; test \"$success\" = true || { echo \"unexpected response: $response\" >&2; false; }".to_string(),
        ],
        env: BTreeMap::new(),
        cwd: None,
    }
}

fn workspace(name: &str) -> (PathBuf, PreparedWorkspace) {
    let root = test_root(name);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("compose.yaml"),
        format!("services:\n  app:\n    image: {TEST_IMAGE}\n    ports:\n      - \"8080:8080\"\n"),
    )
    .unwrap();
    (root.clone(), PreparedWorkspace { workdir: root })
}

fn configure_state(name: &str) -> PathBuf {
    let state = test_root(name);
    fs::create_dir_all(&state).unwrap();
    unsafe {
        std::env::set_var("STATIX_AGENT_STATE_DIR", &state);
    }
    state
}

fn context(name: &str) -> ExecutionContext {
    ExecutionContext {
        job_id: format!("integration-{name}"),
        attempt_id: format!("attempt-{name}"),
        timeout_seconds: 300,
        log_tx: None,
    }
}

#[tokio::test]
#[ignore = "requires privileged LXC tooling and network access"]
async fn lxc_docker_spins_up_executes_and_cleans_up() {
    let state = configure_state("lxc");
    let (workdir, workspace) = workspace("lxc");
    let image =
        std::env::var("STATIX_CONTAINER_TEST_IMAGE").unwrap_or_else(|_| "ubuntu:24.04".to_string());
    let result = execute_spec(
        &RunnerEnvironment::Container {
            image,
            cpu: Some(1),
            memory_mb: Some(512),
        },
        &context("lxc"),
        &workspace,
        compose_command(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    let message = result.message.unwrap();
    assert!(message.contains("ok: success"), "{message}");
    assert!(!state.join("lxc/containers/statix-attempt-lxc").exists());
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(state);
}

#[tokio::test]
#[ignore = "requires KVM/QEMU, a bootable qcow2 image, Docker, and network access"]
async fn microvm_spins_up_executes_and_cleans_up() {
    let base_image = std::env::var("STATIX_MICROVM_TEST_IMAGE")
        .expect("STATIX_MICROVM_TEST_IMAGE must point to a bootable qcow2 image");
    unsafe {
        std::env::set_var("STATIX_MICROVM_BASE_IMAGE", base_image);
    }
    let state = configure_state("microvm");
    let (workdir, workspace) = workspace("microvm");
    let result = execute_spec(
        &RunnerEnvironment::Microvm {
            image: std::env::var("STATIX_MICROVM_TEST_DOCKER_IMAGE")
                .unwrap_or_else(|_| "ubuntu:24.04".to_string()),
            cpu: Some(1),
            memory_mb: Some(1024),
        },
        &context("microvm"),
        &workspace,
        compose_command(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    let message = result.message.unwrap();
    assert!(message.contains("ok: success"), "{message}");
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(state);
}
