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

fn command() -> CommandSpec {
    let mut env = BTreeMap::new();
    env.insert(
        "STATIX_RUNNER_TEST_VALUE".to_string(),
        "from-runner".to_string(),
    );
    CommandSpec {
        argv: vec![
            "sh".to_string(), "-c".to_string(),
            "printf 'stdout:%s:%s' \"$STATIX_RUNNER_TEST_VALUE\" \"$(cat marker)\"; printf 'stderr-line' >&2".to_string(),
        ],
        env,
        cwd: None,
    }
}

fn workspace(name: &str) -> (PathBuf, PreparedWorkspace) {
    let root = test_root(name);
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("marker"), "workspace").unwrap();
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
        command(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    let message = result.message.unwrap();
    assert!(
        message.contains("stdout:from-runner:workspace"),
        "{message}"
    );
    assert!(message.contains("stderr-line"), "{message}");
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
        command(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, "succeeded");
    let message = result.message.unwrap();
    assert!(
        message.contains("stdout:from-runner:workspace"),
        "{message}"
    );
    assert!(message.contains("stderr-line"), "{message}");
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(state);
}
