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
    let root = std::env::var_os("STATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join(format!(
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
    if std::env::var_os("STATIX_RUNNER_TEST_SYSTEMD").is_some() {
        assert!(
            std::env::var_os("INVOCATION_ID").is_some(),
            "not running under systemd"
        );
        assert_eq!(
            std::env::var("STATE_DIRECTORY").unwrap(),
            "/var/lib/statix-agent"
        );
        let user = std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap();
        assert!(user.status.success());
        assert_eq!(String::from_utf8_lossy(&user.stdout).trim(), "statix-agent");
        eprintln!(
            "{name}: running as statix-agent under systemd, state beneath /var/lib/statix-agent"
        );
    }
    let state = test_root(name);
    fs::create_dir_all(&state).unwrap();
    unsafe {
        std::env::set_var("STATIX_AGENT_STATE_DIR", &state);
    }
    state
}

fn context(name: &str, timeout_seconds: u64) -> ExecutionContext {
    ExecutionContext {
        job_id: format!("integration-{name}"),
        attempt_id: format!("attempt-{name}"),
        timeout_seconds,
        log_tx: None,
        log_scope: "job".to_string(),
        log_resource_id: None,
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
        &context("lxc", 600),
        &workspace,
        compose_command(),
    )
    .await
    .unwrap_or_else(|error| panic!("LXC runner failed: {error:#}"));
    assert_eq!(
        result.status, "succeeded",
        "LXC runner: {:?}",
        result.message
    );
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
        // MicroVM provisioning can exceed ten minutes on cold or slow package mirrors.
        &context("microvm", 900),
        &workspace,
        compose_command(),
    )
    .await
    .unwrap_or_else(|error| panic!("MicroVM runner failed: {error:#}"));
    assert_eq!(
        result.status, "succeeded",
        "MicroVM runner: {:?}",
        result.message
    );
    let message = result.message.unwrap();
    assert!(message.contains("ok: success"), "{message}");
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(state);
}

/// Removes the named container even if the test fails partway through.
struct ContainerGuard(String);

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        super::container::lxc::force_destroy(&self.0);
    }
}

/// One blocking HTTP/1.0 GET from the host; Ok(body) on a 200 response.
fn http_get(addr: std::net::SocketAddr) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let timeout = std::time::Duration::from_secs(3);
    let mut stream = std::net::TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(b"GET / HTTP/1.0\r\nHost: statix-test\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.") || !response.contains(" 200 ") {
        return Err(std::io::Error::other(format!(
            "unexpected response: {response:?}"
        )));
    }
    Ok(response)
}

async fn host_get(addr: std::net::SocketAddr, attempts: u32) -> std::io::Result<String> {
    let mut last = None;
    for _ in 0..attempts {
        match tokio::task::spawn_blocking(move || http_get(addr))
            .await
            .unwrap()
        {
            Ok(response) => return Ok(response),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Err(last.unwrap())
}

#[tokio::test]
#[ignore = "requires privileged LXC tooling and network access"]
async fn lxc_8080_ok_is_reachable_from_host() {
    use super::container::{
        archive::{WORKSPACE_ARCHIVE, create_workspace_archive},
        lxc::{LxcContainer, network_command, runtime_ipv4},
    };

    let state = configure_state("lxc-expose");
    let (workdir, ws) = workspace("lxc-expose");

    // Surface guest provisioning output (apt, docker) in the test log.
    let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel::<crate::jobs::JobLogLine>();
    tokio::spawn(async move {
        while let Some(line) = log_rx.recv().await {
            eprintln!("[guest] {}", line.line);
        }
    });
    let mut ctx = context("lxc-expose", 1200);
    ctx.log_tx = Some(log_tx);

    let network = crate::jobs::execute(&RunnerEnvironment::Host, &ctx, &ws, &network_command())
        .await
        .unwrap();
    assert_ne!(network.status, "failed", "{:?}", network.message);

    let name = "statix-expose-test".to_string();
    let _guard = ContainerGuard(name.clone());
    let mut container = LxcContainer::create(name.clone(), "ubuntu", "noble", 2, 2048)
        .await
        .unwrap_or_else(|error| panic!("create failed: {error:#}"));

    // Same sequence as ContainerRunner::execute, but we keep the container
    // alive afterwards so the host can talk to it.
    container.start().await.unwrap();
    container.configure_guest_network(60).await.unwrap();
    container.configure_guest_dns(60).await.unwrap();
    if let Some(failed) = container.prepare_guest(&ctx, 1200, &ws).await.unwrap() {
        panic!("guest provisioning failed: {:?}", failed.message);
    }
    let archive = state.join(WORKSPACE_ARCHIVE);
    create_workspace_archive(&archive, &ws.workdir)
        .await
        .unwrap();
    container.copy_archive_to_guest(&archive).await.unwrap();
    let result = container
        .run_command(&ctx, 600, &compose_command(), &ws)
        .await
        .unwrap();
    assert_eq!(result.status, "succeeded", "{:?}", result.message);

    // The point of the test: the service is reachable from the host over the
    // LXC bridge, not just from inside the guest.
    let ip = runtime_ipv4(&name).await.unwrap();
    let addr = std::net::SocketAddr::from((ip, 8080));
    let response = host_get(addr, 30)
        .await
        .unwrap_or_else(|error| panic!("host could not reach {addr}: {error}"));
    assert!(
        response.ends_with("ok: success\n") || response.contains("ok: success"),
        "{response}"
    );

    // Stopping the container must take the service away (proves we really
    // talked to the guest and not something else on the host).
    container.stop().await.unwrap();
    assert!(http_get(addr).is_err(), "{addr} still answers after stop");

    container.destroy().await;
    assert!(!state.join("lxc/containers").join(&name).exists());
    let _ = fs::remove_dir_all(workdir);
    let _ = fs::remove_dir_all(state);
}

#[tokio::test]
#[ignore = "requires privileged LXC tooling and network access"]
async fn lxc_lifecycle_create_start_stop_start_destroy() {
    use super::container::lxc::{LxcContainer, network_command};

    let state = configure_state("lxc-lifecycle");
    let ctx = context("lxc-lifecycle", 120);
    let ws = PreparedWorkspace {
        workdir: state.clone(),
    };
    let network = crate::jobs::execute(&RunnerEnvironment::Host, &ctx, &ws, &network_command())
        .await
        .unwrap();
    assert_ne!(network.status, "failed", "{:?}", network.message);

    let image =
        std::env::var("STATIX_CONTAINER_TEST_IMAGE").unwrap_or_else(|_| "ubuntu:24.04".to_string());
    let (distribution, release) = image.split_once(':').unwrap();
    // Same distro:release -> template release mapping the production runner uses.
    let release = super::container::image::normalize_release(distribution, release);
    let name = "statix-lifecycle-test".to_string();
    let _guard = ContainerGuard(name.clone());
    let mut container = LxcContainer::create(name.clone(), distribution, release, 1, 512)
        .await
        .unwrap_or_else(|error| panic!("create failed: {error:#}"));
    let rootfs = state.join("lxc/containers").join(&name);
    assert!(rootfs.exists());
    assert_eq!(container.state().await.unwrap(), "STOPPED");

    container.start().await.unwrap();
    assert_eq!(container.state().await.unwrap(), "RUNNING");
    let out = container
        .attach_output(60, "echo marker > /root/marker && echo up", None)
        .await
        .unwrap();
    assert!(out.status.success());

    // Same post-start steps as ContainerRunner::execute: the guest has no IP or
    // resolv.conf until the agent configures them.
    container.configure_guest_network(60).await.unwrap();
    container.configure_guest_dns(60).await.unwrap();

    // Fast connectivity probe: separates guest networking problems (which make
    // the compose test time out in apt) from lifecycle problems.
    let net = container
        .attach_output(
            60,
            "getent hosts archive.ubuntu.com && curl -fsS -m 20 -o /dev/null -w '%{http_code}' http://archive.ubuntu.com/ubuntu/dists/noble/Release",
            None,
        )
        .await
        .unwrap();
    assert!(
        net.status.success(),
        "guest has no working DNS/HTTP: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&net.stdout),
        String::from_utf8_lossy(&net.stderr)
    );

    container.stop().await.unwrap();
    assert_eq!(container.state().await.unwrap(), "STOPPED");

    // Stop must not destroy: the rootfs (and our marker) survive a restart.
    container.start().await.unwrap();
    let out = container
        .attach_output(60, "cat /root/marker", None)
        .await
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "marker");

    container.destroy().await;
    assert!(!rootfs.exists());
    assert!(container.state().await.is_err());
    let _ = fs::remove_dir_all(state);
}
