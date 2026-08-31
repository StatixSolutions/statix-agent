use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use sha2::{Digest, Sha256};
use tokio::{
    process::Command as TokioCommand,
    time::{Duration, sleep, timeout},
};
use tracing::{debug, info, warn};

use crate::config::agent_state_dir;
use crate::jobs::{
    CommandSpec, ExecutionContext, JobExecutionResult, PreparedWorkspace, Runner,
    summarize_command_output,
};

const DEFAULT_SSH_USER: &str = "statix";
const DEFAULT_SSH_PORT: u16 = 2222;
const DEFAULT_DISK_SIZE: &str = "16G";
const DEFAULT_MEMORY_MB: u32 = 4096;
const DEFAULT_CPU_COUNT: u8 = 2;

pub struct MicrovmRunner {
    cpu: Option<u8>,
    memory_mb: Option<u32>,
}

pub struct ProjectMicrovmRunner {
    project_id: String,
    environment: String,
    cpu: Option<u8>,
    memory_mb: Option<u32>,
}

struct QemuProcess {
    child: Option<Child>,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
}

impl QemuProcess {
    fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>> {
        let Some(child) = self.child.as_mut() else {
            return Ok(None);
        };

        let status = child.try_wait().context("failed to poll qemu status")?;
        if status.is_some() {
            self.child.take();
        }

        Ok(status)
    }

    fn failure_message(&self, status: std::process::ExitStatus) -> String {
        let logs = self
            .recent_logs
            .lock()
            .ok()
            .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();

        if logs.trim().is_empty() {
            format!("qemu exited before the guest became ready: {status}")
        } else {
            format!(
                "qemu exited before the guest became ready: {status}\nrecent qemu logs:\n{logs}"
            )
        }
    }

    async fn shutdown(&mut self) {
        if let Ok(Some(status)) = self.try_wait() {
            log_qemu_exit_status(Ok(status));
            return;
        }

        if let Some(mut child) = self.child.take() {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = child.kill();
                log_qemu_exit_status(child.wait());
            })
            .await;
        }
    }
}

impl Drop for QemuProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            log_qemu_exit_status(child.wait());
        }
    }
}

impl MicrovmRunner {
    pub fn new(_image: String, cpu: Option<u8>, memory_mb: Option<u32>) -> Self {
        Self { cpu, memory_mb }
    }
}

impl ProjectMicrovmRunner {
    pub fn new(
        project_id: String,
        environment: String,
        _image: String,
        cpu: Option<u8>,
        memory_mb: Option<u32>,
    ) -> Self {
        Self {
            project_id,
            environment,
            cpu,
            memory_mb,
        }
    }
}

#[async_trait::async_trait]
impl Runner for MicrovmRunner {
    async fn execute(
        &self,
        ctx: &ExecutionContext,
        workspace: &PreparedWorkspace,
        command: &CommandSpec,
    ) -> Result<JobExecutionResult> {
        if ctx.timeout_seconds == 0 || ctx.timeout_seconds > 3600 {
            bail!("run timeoutSeconds must be between 1 and 3600");
        }
        if command.argv.is_empty() {
            bail!("run command must contain at least one token");
        }

        let runtime_root = agent_state_dir()?
            .join("microvm")
            .join(&ctx.job_id)
            .join(&ctx.attempt_id);
        fs::create_dir_all(&runtime_root)
            .with_context(|| format!("failed to create {}", runtime_root.display()))?;
        info!(job_id = %ctx.job_id, runtime_root = %runtime_root.display(), "preparing microvm runtime");

        let base_image = resolve_base_image(
            &std::env::var("STATIX_MICROVM_BASE_IMAGE").unwrap_or_else(|_| "ubuntu-24.04".into()),
            &runtime_root,
        )
        .await?;
        debug!(job_id = %ctx.job_id, base_image = %base_image.display(), "using microvm base image");
        let overlay_image = runtime_root.join("disk.qcow2");
        create_overlay_disk(&base_image, &overlay_image).await?;
        resize_overlay_disk(&overlay_image).await?;
        debug!(job_id = %ctx.job_id, overlay_image = %overlay_image.display(), "created microvm overlay disk");

        let ssh_dir = runtime_root.join("ssh");
        fs::create_dir_all(&ssh_dir)
            .with_context(|| format!("failed to create {}", ssh_dir.display()))?;
        let private_key = ssh_dir.join("id_ed25519");
        let public_key = ssh_dir.join("id_ed25519.pub");
        generate_ssh_keypair(&private_key, &public_key).await?;
        debug!(job_id = %ctx.job_id, "generated microvm ssh keypair");

        let seed_iso = runtime_root.join("seed.iso");
        let workspace_tar = runtime_root.join("workspace.tar.gz");
        create_workspace_archive(&workspace_tar, &workspace.workdir).await?;
        debug!(job_id = %ctx.job_id, workspace = %workspace.workdir.display(), "archived workspace");
        create_cloud_init_seed(&seed_iso, &public_key).await?;
        debug!(job_id = %ctx.job_id, seed = %seed_iso.display(), "created microvm cloud-init seed");

        let mut qemu = launch_qemu(
            &overlay_image,
            &seed_iso,
            self.cpu.unwrap_or(DEFAULT_CPU_COUNT),
            self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB),
            DEFAULT_SSH_PORT,
        )
        .await?;
        info!(job_id = %ctx.job_id, cpu = self.cpu.unwrap_or(DEFAULT_CPU_COUNT), memory_mb = self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB), ssh_port = DEFAULT_SSH_PORT, "launched microvm");

        let result = match wait_for_guest_ready(
            &mut qemu,
            DEFAULT_SSH_PORT,
            &private_key,
            ctx.timeout_seconds,
        )
        .await
        {
            Ok(()) => {
                info!(job_id = %ctx.job_id, "microvm guest is ready");
                run_guest_command(
                    DEFAULT_SSH_PORT,
                    &private_key,
                    ctx.timeout_seconds,
                    command,
                    workspace,
                    &workspace_tar,
                )
                .await
            }
            Err(error) => {
                warn!(job_id = %ctx.job_id, error = %error, "microvm readiness failed");
                Err(error)
            }
        };

        debug!(job_id = %ctx.job_id, "shutting down microvm");
        qemu.shutdown().await;

        result
    }
}

#[async_trait::async_trait]
impl Runner for ProjectMicrovmRunner {
    async fn execute(
        &self,
        ctx: &ExecutionContext,
        workspace: &PreparedWorkspace,
        command: &CommandSpec,
    ) -> Result<JobExecutionResult> {
        if ctx.timeout_seconds == 0 || ctx.timeout_seconds > 3600 {
            bail!("run timeoutSeconds must be between 1 and 3600");
        }
        if command.argv.is_empty() {
            bail!("run command must contain at least one token");
        }

        let vm_key = project_vm_key(&self.project_id, &self.environment);
        let runtime_root = agent_state_dir()?
            .join("microvm")
            .join("projects")
            .join(&vm_key);
        fs::create_dir_all(&runtime_root)
            .with_context(|| format!("failed to create {}", runtime_root.display()))?;
        info!(job_id = %ctx.job_id, vm_key = %vm_key, runtime_root = %runtime_root.display(), "preparing project microvm");

        let base_image = resolve_base_image(
            &std::env::var("STATIX_MICROVM_BASE_IMAGE").unwrap_or_else(|_| "ubuntu-24.04".into()),
            &runtime_root,
        )
        .await?;
        let overlay_image = runtime_root.join("disk.qcow2");
        if !overlay_image.exists() {
            create_overlay_disk(&base_image, &overlay_image).await?;
        }
        resize_overlay_disk(&overlay_image).await?;

        let ssh_dir = runtime_root.join("ssh");
        fs::create_dir_all(&ssh_dir)
            .with_context(|| format!("failed to create {}", ssh_dir.display()))?;
        let private_key = ssh_dir.join("id_ed25519");
        let public_key = ssh_dir.join("id_ed25519.pub");
        if !private_key.exists() || !public_key.exists() {
            generate_ssh_keypair(&private_key, &public_key).await?;
        }

        let seed_iso = runtime_root.join("seed.iso");
        if !seed_iso.exists() {
            create_cloud_init_seed(&seed_iso, &public_key).await?;
        }

        let ssh_port = project_vm_ssh_port(&vm_key);
        if !guest_ready(ssh_port, &private_key).await {
            launch_project_qemu(
                &runtime_root,
                &overlay_image,
                &seed_iso,
                self.cpu.unwrap_or(DEFAULT_CPU_COUNT),
                self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB),
                ssh_port,
            )
            .await?;
        }

        wait_for_project_guest_ready(ssh_port, &private_key, ctx.timeout_seconds).await?;

        let workspace_tar = runtime_root.join(format!(
            "workspace-{}.tar.gz",
            safe_path_segment(&ctx.attempt_id)
        ));
        create_workspace_archive(&workspace_tar, &workspace.workdir).await?;

        let result = run_guest_command(
            ssh_port,
            &private_key,
            ctx.timeout_seconds,
            command,
            workspace,
            &workspace_tar,
        )
        .await;

        let _ = fs::remove_file(workspace_tar);
        result
    }
}

async fn resolve_base_image(image: &str, runtime_root: &Path) -> Result<PathBuf> {
    let trimmed = image.trim();
    if trimmed.is_empty() {
        bail!("microvm image must not be empty");
    }

    let path = Path::new(trimmed);
    if path.exists() {
        return Ok(path.to_path_buf());
    }

    let url = match trimmed {
        "ubuntu-24.04" | "ubuntu-24.04-lts" | "ubuntu-noble" => canonical_ubuntu_image_url(),
        value if value.starts_with("http://") || value.starts_with("https://") => value.to_owned(),
        value => bail!("unsupported microvm image reference: {value}"),
    };

    let cache_dir = runtime_root.join("images");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create {}", cache_dir.display()))?;
    let cache_name = format!("{}.img", image_cache_key(&url));
    let cached_image = cache_dir.join(cache_name);
    if cached_image.exists() {
        return Ok(cached_image);
    }

    let bytes = reqwest::get(&url)
        .await
        .with_context(|| format!("failed to download microvm image from {url}"))?
        .error_for_status()
        .with_context(|| format!("microvm image request failed for {url}"))?
        .bytes()
        .await
        .with_context(|| format!("failed to read microvm image from {url}"))?;

    fs::write(&cached_image, &bytes).with_context(|| {
        format!(
            "failed to cache microvm image at {}",
            cached_image.display()
        )
    })?;
    Ok(cached_image)
}

async fn create_overlay_disk(base_image: &Path, overlay_image: &Path) -> Result<()> {
    let status = TokioCommand::new("qemu-img")
        .arg("create")
        .arg("-f")
        .arg("qcow2")
        .arg("-F")
        .arg("qcow2")
        .arg("-b")
        .arg(base_image)
        .arg(overlay_image)
        .status()
        .await
        .with_context(|| missing_dependency_message("qemu-img", "qemu-utils"))?;

    if !status.success() {
        bail!("qemu-img failed to create overlay disk");
    }

    Ok(())
}

async fn resize_overlay_disk(overlay_image: &Path) -> Result<()> {
    let status = TokioCommand::new("qemu-img")
        .arg("resize")
        .arg(overlay_image)
        .arg(DEFAULT_DISK_SIZE)
        .status()
        .await
        .with_context(|| missing_dependency_message("qemu-img", "qemu-utils"))?;

    if !status.success() {
        bail!("qemu-img failed to resize the microvm overlay disk");
    }
    Ok(())
}

async fn generate_ssh_keypair(private_key: &Path, public_key: &Path) -> Result<()> {
    if private_key.exists() {
        let _ = fs::remove_file(private_key);
    }
    if public_key.exists() {
        let _ = fs::remove_file(public_key);
    }

    let status = TokioCommand::new("ssh-keygen")
        .arg("-q")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-f")
        .arg(private_key)
        .status()
        .await
        .context("failed to launch ssh-keygen")?;

    if !status.success() {
        bail!("ssh-keygen failed to create guest keypair");
    }

    if !public_key.exists() {
        bail!("ssh-keygen did not create a public key");
    }

    Ok(())
}

async fn create_workspace_archive(archive_path: &Path, workdir: &Path) -> Result<()> {
    if archive_path.exists() {
        let _ = fs::remove_file(archive_path);
    }

    let status = TokioCommand::new("tar")
        .arg("-C")
        .arg(workdir)
        .arg("-czf")
        .arg(archive_path)
        .arg(".")
        .status()
        .await
        .context("failed to launch tar")?;

    if !status.success() {
        bail!("failed to archive workspace for microvm execution");
    }

    Ok(())
}

async fn create_cloud_init_seed(seed_iso: &Path, public_key: &Path) -> Result<()> {
    let public_key = fs::read_to_string(public_key)
        .with_context(|| format!("failed to read {}", public_key.display()))?;

    let user_data = format!(
        r#"#cloud-config
users:
  - name: {user}
    groups: [sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - {public_key}
ssh_pwauth: false
disable_root: true
package_update: true
packages:
  - build-essential
  - ca-certificates
  - curl
  - docker.io
  - docker-compose-v2
  - git
  - libssl-dev
  - openssh-server
  - pkg-config
runcmd:
  - [bash, -lc, "id -u {user} >/dev/null 2>&1 || useradd --create-home --shell /bin/bash {user}"]
  - [systemctl, enable, --now, docker]
  - [usermod, -aG, docker, "{user}"]
  - [install, -d, -o, "{user}", -g, "{user}", -m, "0755", "/home/{user}/docker"]
  - [su, "-", "{user}", "-c", "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable"]
"#,
        user = DEFAULT_SSH_USER,
        public_key = public_key.trim(),
    );

    let meta_data = "instance-id: statix-microvm\nlocal-hostname: statix-microvm\n";
    let user_data_path = seed_iso.with_extension("user-data");
    let meta_data_path = seed_iso.with_extension("meta-data");
    fs::write(&user_data_path, user_data)
        .with_context(|| format!("failed to write {}", user_data_path.display()))?;
    fs::write(&meta_data_path, meta_data)
        .with_context(|| format!("failed to write {}", meta_data_path.display()))?;

    let status = TokioCommand::new("cloud-localds")
        .arg(seed_iso)
        .arg(&user_data_path)
        .arg(&meta_data_path)
        .status()
        .await
        .with_context(|| missing_dependency_message("cloud-localds", "cloud-image-utils"))?;

    let _ = fs::remove_file(user_data_path);
    let _ = fs::remove_file(meta_data_path);

    if !status.success() {
        bail!("cloud-localds failed to create the seed image");
    }

    Ok(())
}

async fn launch_qemu(
    overlay_image: &Path,
    seed_iso: &Path,
    cpu: u8,
    memory_mb: u32,
    ssh_port: u16,
) -> Result<QemuProcess> {
    let binary = qemu_binary()?;
    let recent_logs = Arc::new(Mutex::new(VecDeque::with_capacity(32)));
    let mut command = Command::new(binary);
    command.args(qemu_launch_args(
        overlay_image,
        seed_iso,
        cpu,
        memory_mb,
        ssh_port,
    ));

    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| missing_dependency_message(binary, qemu_package_hint()))?;
    spawn_qemu_log_stream("stdout", child.stdout.take(), Arc::clone(&recent_logs));
    spawn_qemu_log_stream("stderr", child.stderr.take(), Arc::clone(&recent_logs));
    Ok(QemuProcess {
        child: Some(child),
        recent_logs,
    })
}

async fn launch_project_qemu(
    runtime_root: &Path,
    overlay_image: &Path,
    seed_iso: &Path,
    cpu: u8,
    memory_mb: u32,
    ssh_port: u16,
) -> Result<()> {
    let binary = qemu_binary()?;
    let stdout = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime_root.join("qemu.stdout.log"))
        .with_context(|| {
            format!(
                "failed to open {}",
                runtime_root.join("qemu.stdout.log").display()
            )
        })?;
    let stderr = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(runtime_root.join("qemu.stderr.log"))
        .with_context(|| {
            format!(
                "failed to open {}",
                runtime_root.join("qemu.stderr.log").display()
            )
        })?;
    let child = Command::new(binary)
        .args(qemu_launch_args(
            overlay_image,
            seed_iso,
            cpu,
            memory_mb,
            ssh_port,
        ))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| missing_dependency_message(binary, qemu_package_hint()))?;

    fs::write(runtime_root.join("qemu.pid"), child.id().to_string()).with_context(|| {
        format!(
            "failed to write {}",
            runtime_root.join("qemu.pid").display()
        )
    })?;
    info!(
        pid = child.id(),
        cpu, memory_mb, ssh_port, "launched project microvm"
    );
    Ok(())
}

fn qemu_launch_args(
    overlay_image: &Path,
    seed_iso: &Path,
    cpu: u8,
    memory_mb: u32,
    ssh_port: u16,
) -> Vec<String> {
    vec![
        "-m".to_string(),
        memory_mb.to_string(),
        "-smp".to_string(),
        cpu.to_string(),
        "-accel".to_string(),
        "kvm".to_string(),
        "-accel".to_string(),
        "tcg,split-wx=on,thread=multi".to_string(),
        "-drive".to_string(),
        format!("if=virtio,format=qcow2,file={}", overlay_image.display()),
        "-drive".to_string(),
        format!(
            "if=virtio,format=raw,file={},media=cdrom",
            seed_iso.display()
        ),
        "-netdev".to_string(),
        format!("user,id=net0,hostfwd=tcp::{}-:22", ssh_port),
        "-device".to_string(),
        "virtio-net-pci,netdev=net0".to_string(),
        "-nographic".to_string(),
    ]
}

fn spawn_qemu_log_stream(
    label: &'static str,
    stream: Option<impl std::io::Read + Send + 'static>,
    recent_logs: Arc<Mutex<VecDeque<String>>>,
) {
    let Some(stream) = stream else {
        return;
    };

    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if let Ok(mut logs) = recent_logs.lock() {
                        if logs.len() == 32 {
                            logs.pop_front();
                        }
                        logs.push_back(format!("{label}: {line}"));
                    }

                    debug!(stream = label, line = %truncate_for_log(&line, 1_000), "qemu output");
                }
                Err(error) => {
                    warn!(stream = label, error = %error, "qemu log read failed");
                    break;
                }
            }
        }
    });
}

fn log_qemu_exit_status(result: std::io::Result<std::process::ExitStatus>) {
    match result {
        Ok(status) if status.success() => {
            debug!(%status, "qemu exited successfully");
        }
        Ok(status) => warn!(%status, "qemu exited unsuccessfully"),
        Err(error) => warn!(error = %error, "failed to wait for qemu"),
    }
}

async fn wait_for_guest_ready(
    qemu: &mut QemuProcess,
    ssh_port: u16,
    private_key: &Path,
    timeout_seconds: u64,
) -> Result<()> {
    let deadline = Duration::from_secs(timeout_seconds);
    let start = Instant::now();
    let mut next_progress_log = Duration::from_secs(15);

    loop {
        if let Some(status) = qemu.try_wait()? {
            bail!(qemu.failure_message(status));
        }

        if start.elapsed() >= deadline {
            bail!("microvm did not become ready before timeout");
        }

        let remaining = deadline.saturating_sub(start.elapsed());
        let mut probe = TokioCommand::new("ssh");
        probe.arg("-i").arg(private_key);
        add_common_ssh_options(&mut probe, 3);
        probe
            .arg("-p")
            .arg(ssh_port.to_string())
            .arg(format!("{}@127.0.0.1", DEFAULT_SSH_USER))
            .arg("test")
            .arg("-f")
            .arg("/var/lib/cloud/instance/boot-finished");

        let last_probe_failure =
            match timeout(remaining.min(Duration::from_secs(5)), probe.output()).await {
                Ok(Ok(output)) if output.status.success() => return Ok(()),
                Ok(Ok(output)) => readiness_probe_failure(&output),
                Ok(Err(error)) => format!("failed to launch ssh readiness probe: {error}"),
                Err(_) => "ssh readiness probe timed out".to_string(),
            };

        let elapsed = start.elapsed();
        if elapsed >= next_progress_log {
            debug!(elapsed_seconds = elapsed.as_secs(), last_probe_failure = %last_probe_failure, "waiting for microvm guest readiness");
            next_progress_log += Duration::from_secs(15);
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn wait_for_project_guest_ready(
    ssh_port: u16,
    private_key: &Path,
    timeout_seconds: u64,
) -> Result<()> {
    let deadline = Duration::from_secs(timeout_seconds);
    let start = Instant::now();
    let mut next_progress_log = Duration::from_secs(15);

    loop {
        if start.elapsed() >= deadline {
            bail!("project microvm did not become ready before timeout");
        }

        if guest_ready(ssh_port, private_key).await {
            return Ok(());
        }

        let elapsed = start.elapsed();
        if elapsed >= next_progress_log {
            debug!(
                elapsed_seconds = elapsed.as_secs(),
                "waiting for project microvm guest readiness"
            );
            next_progress_log += Duration::from_secs(15);
        }

        sleep(Duration::from_secs(2)).await;
    }
}

async fn guest_ready(ssh_port: u16, private_key: &Path) -> bool {
    let mut probe = TokioCommand::new("ssh");
    probe.arg("-i").arg(private_key);
    add_common_ssh_options(&mut probe, 3);
    probe
        .arg("-p")
        .arg(ssh_port.to_string())
        .arg(format!("{}@127.0.0.1", DEFAULT_SSH_USER))
        .arg("test")
        .arg("-f")
        .arg("/var/lib/cloud/instance/boot-finished");

    matches!(
        timeout(Duration::from_secs(5), probe.output()).await,
        Ok(Ok(output)) if output.status.success()
    )
}

async fn run_guest_command(
    ssh_port: u16,
    private_key: &Path,
    timeout_seconds: u64,
    command: &CommandSpec,
    workspace: &PreparedWorkspace,
    workspace_tar: &Path,
) -> Result<JobExecutionResult> {
    debug!(workspace_archive = %workspace_tar.display(), "uploading workspace archive to microvm");
    let upload = timeout(Duration::from_secs(timeout_seconds), async {
        let mut scp = TokioCommand::new("scp");
        scp.arg("-i").arg(private_key);
        add_common_ssh_options(&mut scp, 5);
        scp.arg("-P")
            .arg(ssh_port.to_string())
            .arg(workspace_tar)
            .arg(format!(
                "{}@127.0.0.1:/home/{}/workspace.tar.gz",
                DEFAULT_SSH_USER, DEFAULT_SSH_USER
            ));
        scp.output().await
    })
    .await
    .map_err(|_| {
        anyhow!(
            "microvm archive upload timed out after {} seconds",
            timeout_seconds
        )
    })?
    .map_err(anyhow::Error::from)
    .map_err(|error| error.context("failed to upload workspace archive to microvm"))?;

    if !upload.status.success() {
        warn!(status = %upload.status, "microvm workspace archive upload failed");
        return Ok(JobExecutionResult {
            status: "failed",
            message: Some(summarize_command_output(
                &workspace.workdir,
                &upload.stdout,
                &upload.stderr,
            )),
        });
    }
    debug!("uploaded workspace archive to microvm");

    let env = command
        .env
        .iter()
        .map(|(key, value)| format!("export {}={};", shell_env_key(key), shell_escape(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = command.cwd.as_deref().unwrap_or("/home/statix/docker");
    let cwd = if cwd == "/workspace" {
        "/home/statix/docker"
    } else {
        cwd
    };
    if !cwd.starts_with("/home/statix/docker") {
        bail!("microvm command cwd must be inside /home/statix/docker");
    }
    let remote_command = format!(
        "set -e; if ! command -v docker >/dev/null 2>&1 || ! sudo docker info >/dev/null 2>&1; then sudo apt-get update && sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-v2 curl; sudo systemctl enable --now docker; fi; sudo usermod --append --groups docker {user}; sudo install -d -o {user} -g {user} -m 0755 /home/{user}/docker; find /home/{user}/docker -mindepth 1 -maxdepth 1 -exec rm -rf -- {{}} +; tar -xzf /home/{user}/workspace.tar.gz -C /home/{user}/docker; sudo chown -R {user}:{user} /home/{user}/docker; sg docker -c {user_command}",
        user = DEFAULT_SSH_USER,
        user_command = shell_escape(&format!(
            "cd {} && {} exec {}",
            shell_escape(cwd),
            env,
            shell_join(&command.argv)
        )),
    );

    info!(command = %redacted_command(&command.argv), "running command inside microvm");
    let output = timeout(Duration::from_secs(timeout_seconds), async {
        let mut ssh = TokioCommand::new("ssh");
        ssh.arg("-i").arg(private_key);
        add_common_ssh_options(&mut ssh, 5);
        ssh.arg("-p")
            .arg(ssh_port.to_string())
            .arg(format!("{}@127.0.0.1", DEFAULT_SSH_USER))
            .arg(remote_command)
            .current_dir(&workspace.workdir);
        ssh.output().await
    })
    .await
    .map_err(|_| {
        anyhow!(
            "microvm command timed out after {} seconds",
            timeout_seconds
        )
    })?
    .map_err(anyhow::Error::from)
    .map_err(|error| error.context("failed to execute command inside microvm"))?;

    let message = summarize_command_output(&workspace.workdir, &output.stdout, &output.stderr);
    if output.status.success() {
        info!(status = "succeeded", "microvm command finished");
        Ok(JobExecutionResult {
            status: "succeeded",
            message: Some(message),
        })
    } else {
        warn!(status = %output.status, output = %truncate_for_log(&message, 1_000), "microvm command failed");
        Ok(JobExecutionResult {
            status: "failed",
            message: Some(message),
        })
    }
}

fn add_common_ssh_options(command: &mut TokioCommand, connect_timeout_seconds: u64) {
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={connect_timeout_seconds}"))
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("GlobalKnownHostsFile=/dev/null")
        .arg("-o")
        .arg("LogLevel=ERROR");
}

fn readiness_probe_failure(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.code() == Some(1) && readiness_stderr_is_non_actionable(&stderr) {
        return "ssh is reachable; waiting for cloud-init boot-finished marker".to_string();
    }

    if stderr.is_empty() {
        format!("ssh readiness probe exited with {}", output.status)
    } else {
        format!(
            "ssh readiness probe exited with {}: {}",
            output.status,
            truncate_for_log(&stderr, 240)
        )
    }
}

fn readiness_stderr_is_non_actionable(stderr: &str) -> bool {
    stderr.is_empty() || stderr.starts_with("Warning: Permanently added ")
}

fn truncate_for_log(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }

    let mut shortened = value.chars().take(max_chars).collect::<String>();
    shortened.push_str("...");
    shortened
}

fn shell_join(command: &[String]) -> String {
    command
        .iter()
        .map(|value| shell_escape(value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn redacted_command(command: &[String]) -> String {
    command
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let sensitive = index > 0
                && matches!(
                    command[index - 1].to_ascii_lowercase().as_str(),
                    "--token" | "--password" | "--secret" | "--api-key" | "-p"
                );
            if sensitive {
                "<redacted>".to_owned()
            } else {
                shell_escape(value)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_env_key(key: &str) -> String {
    if key
        .chars()
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && key.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        key.to_owned()
    } else {
        "STATIX_INVALID_ENV".to_owned()
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%_-+=:,./".contains(c))
    {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn qemu_binary() -> Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("qemu-system-x86_64"),
        "aarch64" => Ok("qemu-system-aarch64"),
        arch => bail!("unsupported host architecture for microvm runner: {arch}"),
    }
}

fn qemu_package_hint() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "qemu-system-x86",
        "aarch64" => "qemu-system-arm",
        _ => "qemu-system",
    }
}

fn missing_dependency_message(program: &str, debian_package: &str) -> String {
    format!(
        "failed to launch {program}; install the '{debian_package}' package and ensure {program} is on PATH"
    )
}

fn canonical_ubuntu_image_url() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
            .to_string(),
        "aarch64" => {
            "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-arm64.img"
                .to_string()
        }
        _ => "https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img"
            .to_string(),
    }
}

fn image_cache_key(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hex::encode(hasher.finalize())
}

fn project_vm_key(project_id: &str, environment: &str) -> String {
    format!(
        "{}-{}-{}",
        safe_path_segment(project_id),
        safe_path_segment(environment),
        image_cache_key(&format!("{project_id}:{environment}"))[..12].to_string()
    )
}

fn project_vm_ssh_port(vm_key: &str) -> u16 {
    let mut hasher = Sha256::new();
    hasher.update(vm_key.as_bytes());
    let digest = hasher.finalize();
    let value = u16::from_be_bytes([digest[0], digest[1]]);
    22_200 + (value % 1_000)
}

fn safe_path_segment(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_launch_args_enable_kvm_fallback_and_split_wx_tcg() {
        let overlay_image = Path::new("/tmp/disk.qcow2");
        let seed_iso = Path::new("/tmp/seed.iso");

        let args = qemu_launch_args(overlay_image, seed_iso, 4, 2048, 2222);

        assert_eq!(
            args,
            vec![
                "-m",
                "2048",
                "-smp",
                "4",
                "-accel",
                "kvm",
                "-accel",
                "tcg,split-wx=on,thread=multi",
                "-drive",
                "if=virtio,format=qcow2,file=/tmp/disk.qcow2",
                "-drive",
                "if=virtio,format=raw,file=/tmp/seed.iso,media=cdrom",
                "-netdev",
                "user,id=net0,hostfwd=tcp::2222-:22",
                "-device",
                "virtio-net-pci,netdev=net0",
                "-nographic",
            ]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn readiness_known_host_warning_is_non_actionable() {
        assert!(readiness_stderr_is_non_actionable(""));
        assert!(readiness_stderr_is_non_actionable(
            "Warning: Permanently added '[127.0.0.1]:2222' (ED25519) to the list of known hosts."
        ));
        assert!(!readiness_stderr_is_non_actionable(
            "Permission denied (publickey)."
        ));
    }
}
