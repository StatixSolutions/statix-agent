use std::{
    env, fs,
    io::Write,
    net::Ipv4Addr,
    path::{Path, PathBuf},
    process::{Command as StdCommand, Output, Stdio},
};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{
    io::AsyncReadExt,
    process::Command as TokioCommand,
    time::{Duration, timeout},
};
use tracing::{debug, info, warn};

use crate::jobs::{
    CommandSpec, ExecutionContext, JobExecutionResult, JobLogStream, PreparedWorkspace,
    summarize_command_output,
};

use super::{
    archive::WORKSPACE_ARCHIVE,
    diagnostics::{
        lxc_log_excerpt, lxc_start_failure_message, missing_dependency_message,
        summarize_raw_command_output,
    },
    dns::{container_dns_config, guest_resolv_conf_command},
    image::lxc_arch,
    network::{guest_ipv4_address, guest_network_command, lxc_bridge_network},
    shell::{shell_escape, shell_join, truncate_for_log},
};

pub(super) struct LxcContainer {
    name: String,
    destroyed: bool,
}

impl LxcContainer {
    fn check_dependencies() -> Result<()> {
        let required_cmds = [
            "sudo",
            "lxc-create",
            "lxc-start",
            "lxc-wait",
            "lxc-attach",
            "lxc-stop",
            "lxc-destroy",
        ];
        let mut missing = Vec::new();

        for cmd in required_cmds.iter() {
            if let Err(e) = StdCommand::new(cmd).arg("--version").output() {
                if e.kind() == std::io::ErrorKind::NotFound {
                    missing.push(*cmd);
                }
            }
        }

        if !missing.is_empty() {
            bail!(
                "Missing required LXC dependencies: {}. Please install the 'lxc' package before creating containers.",
                missing.join(", ")
            );
        }

        if !Path::new(lxc_helper_path()).is_file() {
            bail!(
                "Missing LXC privilege helper at {}; reinstall statix-agent",
                lxc_helper_path()
            );
        }

        Ok(())
    }

    pub(super) async fn create(
        name: String,
        distribution: &str,
        release: &str,
        cpu: u8,
        memory_mb: u32,
    ) -> Result<Self> {
        Self::check_dependencies()?;

        ensure_lxc_directory_permissions()?;

        info!(
            container = %name,
            distribution,
            release,
            architecture = lxc_arch(),
            variant = "default",
            "creating lxc container from image template"
        );

        let lxc_path = lxc_storage_path();
        fs::create_dir_all(&lxc_path)
            .with_context(|| format!("failed to create {}", lxc_path.display()))?;

        let log_path = lxc_path.join(format!("{name}.create.log"));
        let status = lxc_command("lxc-create")
            .arg("-n")
            .arg(&name)
            .arg("-P")
            .arg(&lxc_path)
            .arg("--logfile")
            .arg(&log_path)
            .arg("--logpriority")
            .arg("DEBUG")
            .arg("-t")
            .arg("download")
            .arg("--")
            .arg("-d")
            .arg(distribution)
            .arg("-r")
            .arg(release)
            .arg("-a")
            .arg(lxc_arch())
            .status()
            .await
            .with_context(|| missing_dependency_message("lxc-create", "lxc"))?;

        if !status.success() {
            bail!(
                "lxc-create failed for container {name} with {status} (requested {distribution}:{release}, arch {}): {}. Verify the lxc-download template and image availability with `lxc-create -t download -n probe -- --list`",
                lxc_arch(),
                lxc_log_excerpt(&log_path),
            );
        }

        let container = Self {
            name,
            destroyed: false,
        };
        container.apply_job_config(cpu, memory_mb, enforce_lxc_limits())?;
        if lxc_bridge_network().is_none() {
            container.append_networkless_config()?;
        }
        Ok(container)
    }

    pub(super) async fn start(&mut self) -> Result<()> {
        let log_path = self.log_path();
        let status = lxc_command("lxc-start")
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .arg("--logfile")
            .arg(&log_path)
            .arg("--logpriority")
            .arg("DEBUG")
            .arg("-d")
            .status()
            .await
            .with_context(|| missing_dependency_message("lxc-start", "lxc"))?;

        if !status.success() {
            let excerpt = lxc_log_excerpt(&log_path);
            bail!(
                "lxc-start failed for container {} with {status}: {}",
                self.name,
                lxc_start_failure_message(&excerpt)
            );
        }

        let status = lxc_command("lxc-wait")
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .arg("-s")
            .arg("RUNNING")
            .arg("-t")
            .arg("30")
            .status()
            .await
            .with_context(|| missing_dependency_message("lxc-wait", "lxc"))?;

        if !status.success() {
            bail!(
                "lxc-wait did not observe container {} running: {status}: {}",
                self.name,
                lxc_log_excerpt(&log_path)
            );
        }

        Ok(())
    }

    pub(super) async fn copy_archive_to_guest(&self, archive_path: &Path) -> Result<()> {
        let archive = fs::read(archive_path).with_context(|| {
            format!(
                "failed to read workspace archive {}",
                archive_path.display()
            )
        })?;

        let mut process = lxc_std_command("lxc-attach");
        process
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg(format!("cat > /tmp/{WORKSPACE_ARCHIVE}"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = process.spawn().with_context(|| {
            format!(
                "failed to start archive copy into lxc container {}",
                self.name
            )
        })?;
        let mut stdin = child
            .stdin
            .take()
            .context("failed to open lxc-attach stdin")?;
        std::thread::spawn(move || {
            use std::io::Write;
            stdin.write_all(&archive)
        })
        .join()
        .map_err(|_| anyhow!("archive copy writer thread panicked"))?
        .context("failed to write workspace archive into lxc container")?;

        let output = child
            .wait_with_output()
            .context("failed to wait for workspace archive copy")?;
        if !output.status.success() {
            bail!(
                "failed to copy workspace archive into lxc container with {}: {}",
                output.status,
                summarize_raw_command_output(&output.stdout, &output.stderr)
            );
        }
        Ok(())
    }

    pub(super) async fn configure_guest_dns(&self, timeout_seconds: u64) -> Result<()> {
        let dns_config = container_dns_config();
        if dns_config.nameservers.is_empty() && !dns_config.include_default_gateway {
            warn!(container = %self.name, "no non-loopback DNS resolvers found for guest");
            return Ok(());
        }

        let command = guest_resolv_conf_command(&dns_config);
        let output = self.attach_output(timeout_seconds, &command, None).await?;
        if !output.status.success() {
            bail!(
                "failed to configure lxc container DNS with {}: {}",
                output.status,
                summarize_raw_command_output(&output.stdout, &output.stderr)
            );
        }
        debug!(container = %self.name, resolvers = %dns_config.display_nameservers(), "configured guest DNS resolvers");
        Ok(())
    }

    pub(super) async fn configure_guest_network(&self, timeout_seconds: u64) -> Result<()> {
        let Some(network) = lxc_bridge_network() else {
            warn!(container = %self.name, "could not detect lxc bridge IPv4 network; leaving guest network unchanged");
            return Ok(());
        };
        let guest_address = guest_ipv4_address(&network, &self.name);
        let command = guest_network_command(&network, guest_address);

        let output = self.attach_output(timeout_seconds, &command, None).await?;
        if !output.status.success() {
            bail!(
                "failed to configure lxc container network with {}: {}",
                output.status,
                summarize_raw_command_output(&output.stdout, &output.stderr)
            );
        }
        debug!(container = %self.name, guest_address = %guest_address, gateway = %network.gateway, "ensured guest IPv4 network");
        Ok(())
    }

    pub(super) async fn prepare_guest(
        &self,
        ctx: &ExecutionContext,
        timeout_seconds: u64,
        workspace: &PreparedWorkspace,
    ) -> Result<Option<JobExecutionResult>> {
        let setup_command = concat!(
            "set -e; ",
            "echo '[statix-agent] guest network diagnostics:'; ",
            "echo '[statix-agent] ip addr:'; ip addr || true; ",
            "echo '[statix-agent] ip route:'; ip route || true; ",
            "echo '[statix-agent] /etc/resolv.conf:'; cat /etc/resolv.conf || true; ",
            "command -v apt-get >/dev/null 2>&1 || { echo '[statix-agent] Docker provisioning currently requires an apt-based LXC guest' >&2; exit 1; }; ",
            "if ! id statix >/dev/null 2>&1; then useradd --create-home --shell /bin/bash statix; fi; ",
            "install -d -o statix -g statix -m 0755 /home/statix/docker; ",
            "echo '[statix-agent] apt-get update'; apt-get update; ",
            "echo '[statix-agent] installing Docker, Compose, and build dependencies'; ",
            "DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-v2 build-essential ca-certificates curl git libssl-dev pkg-config; ",
            // Overlay mounts are not available inside the nested LXC used by the
            // runner.  vfs keeps Docker fully functional without requiring a
            // nested overlayfs mount.
            "install -d /etc/docker; printf '%s\\n' '{\"storage-driver\":\"vfs\"}' > /etc/docker/daemon.json; ",
            "systemctl enable --now docker; systemctl restart docker; ",
            "usermod --append --groups docker statix; ",
            "docker info >/dev/null; ",
            "if command -v cargo >/dev/null 2>&1; then ",
            "echo '[statix-agent] cargo already available'; ",
            "else ",
            "echo '[statix-agent] installing rust toolchain'; ",
            "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable; ",
            "fi"
        );
        let output = self
            .attach_output(timeout_seconds, setup_command, Some(ctx))
            .await?;
        if !output.status.success() {
            let message =
                summarize_command_output(&workspace.workdir, &output.stdout, &output.stderr);
            warn!(container = %self.name, status = %output.status, output = %truncate_for_log(&message, 1_000), "lxc container setup failed");
            return Ok(Some(JobExecutionResult {
                status: "failed",
                message: Some(message),
            }));
        }
        Ok(None)
    }

    pub(super) async fn run_command(
        &self,
        ctx: &ExecutionContext,
        timeout_seconds: u64,
        command: &CommandSpec,
        workspace: &PreparedWorkspace,
    ) -> Result<JobExecutionResult> {
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
            bail!("container command cwd must be inside /home/statix/docker");
        }
        let guest_command = format!(
            "rm -rf /home/statix/docker && mkdir -p /home/statix/docker && tar -xzf /tmp/{archive} -C /home/statix/docker && chown -R statix:statix /home/statix/docker && su -s /bin/bash - statix -c {user_command}",
            archive = WORKSPACE_ARCHIVE,
            user_command = shell_escape(&format!(
                "cd {} && {} exec {}",
                shell_escape(cwd),
                env,
                shell_join(&command.argv)
            )),
        );

        info!(container = %self.name, command = %redacted_command(&command.argv), "running command inside lxc container");
        let output = self
            .attach_output(timeout_seconds, &guest_command, Some(ctx))
            .await?;
        if output.status.success() {
            info!(container = %self.name, status = "succeeded", "lxc container command finished");
            Ok(JobExecutionResult {
                status: "succeeded",
                message: Some(summarize_command_output(
                    &workspace.workdir,
                    &output.stdout,
                    &output.stderr,
                )),
            })
        } else {
            let message =
                summarize_command_output(&workspace.workdir, &output.stdout, &output.stderr);
            warn!(container = %self.name, status = %output.status, output = %truncate_for_log(&message, 1_000), "lxc container command failed");
            Ok(JobExecutionResult {
                status: "failed",
                message: Some(message),
            })
        }
    }

    async fn attach_output(
        &self,
        timeout_seconds: u64,
        shell_command: &str,
        ctx: Option<&ExecutionContext>,
    ) -> Result<Output> {
        let mut process = lxc_command("lxc-attach");
        process
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .arg("--")
            .arg("sh")
            .arg("-lc")
            .arg(shell_command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = process
            .spawn()
            .map_err(anyhow::Error::from)
            .map_err(|error| {
                error.context(format!(
                    "failed to execute command inside lxc container {}",
                    self.name
                ))
            })?;

        let stdout = child
            .stdout
            .take()
            .context("failed to capture lxc-attach stdout")?;
        let stderr = child
            .stderr
            .take()
            .context("failed to capture lxc-attach stderr")?;

        let container_name_for_stdout = self.name.clone();
        let stdout_ctx = ctx.cloned();
        let stdout_task = tokio::spawn(async move {
            stream_and_collect_output(
                stdout,
                &container_name_for_stdout,
                JobLogStream::Stdout,
                stdout_ctx,
            )
            .await
        });

        let container_name_for_stderr = self.name.clone();
        let stderr_ctx = ctx.cloned();
        let stderr_task = tokio::spawn(async move {
            stream_and_collect_output(
                stderr,
                &container_name_for_stderr,
                JobLogStream::Stderr,
                stderr_ctx,
            )
            .await
        });

        let status = match timeout(Duration::from_secs(timeout_seconds), child.wait()).await {
            Ok(wait_result) => wait_result.map_err(anyhow::Error::from).map_err(|error| {
                error.context(format!(
                    "failed to execute command inside lxc container {}",
                    self.name
                ))
            })?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                bail!("lxc command timed out after {} seconds", timeout_seconds);
            }
        };

        let stdout = stdout_task
            .await
            .context("stdout streaming task join failure")??;
        let stderr = stderr_task
            .await
            .context("stderr streaming task join failure")??;

        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }

    pub(super) async fn destroy(&mut self) {
        if self.destroyed {
            return;
        }

        let _ = lxc_command("lxc-stop")
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .arg("--kill")
            .status()
            .await;
        let _ = lxc_command("lxc-destroy")
            .arg("-n")
            .arg(&self.name)
            .arg("-P")
            .arg(lxc_storage_path())
            .status()
            .await;
        self.destroyed = true;
    }

    fn config_path(&self) -> PathBuf {
        lxc_storage_path().join(&self.name).join("config")
    }

    fn log_path(&self) -> PathBuf {
        lxc_storage_path().join(&self.name).join("statix-lxc.log")
    }

    fn apply_job_config(&self, cpu: u8, memory_mb: u32, enforce_limits: bool) -> Result<()> {
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(self.config_path())
            .with_context(|| format!("failed to open lxc config for {}", self.name))?;

        write_job_lxc_config(&mut config, cpu, memory_mb, enforce_limits)?;
        Ok(())
    }

    fn append_networkless_config(&self) -> Result<()> {
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(self.config_path())
            .with_context(|| format!("failed to open lxc config for {}", self.name))?;
        writeln!(config, "\n# Statix fallback when lxcbr0 is unavailable")?;
        writeln!(config, "lxc.net.0.type = empty")?;
        Ok(())
    }
}

async fn stream_and_collect_output(
    stream: impl tokio::io::AsyncRead + Unpin,
    container_name: &str,
    stream_name: JobLogStream,
    ctx: Option<ExecutionContext>,
) -> Result<Vec<u8>> {
    let mut reader = stream;
    let mut buffer = Vec::new();
    let mut pending = Vec::new();
    let mut chunk = [0_u8; 8192];

    loop {
        let bytes_read = reader
            .read(&mut chunk)
            .await
            .context("failed to read streamed output from lxc-attach")?;

        if bytes_read == 0 {
            break;
        }

        for byte in &chunk[..bytes_read] {
            buffer.push(*byte);
            if matches!(*byte, b'\n' | b'\r') {
                emit_stream_segment(container_name, stream_name, &pending, ctx.as_ref());
                pending.clear();
            } else {
                pending.push(*byte);
            }
        }
    }

    emit_stream_segment(container_name, stream_name, &pending, ctx.as_ref());

    Ok(buffer)
}

fn emit_stream_segment(
    container_name: &str,
    stream_name: JobLogStream,
    segment: &[u8],
    ctx: Option<&ExecutionContext>,
) {
    if segment.is_empty() {
        return;
    }

    let message = String::from_utf8_lossy(segment);
    debug!(container = %container_name, stream = stream_name.as_str(), output = %truncate_for_log(&message, 1_000), "lxc container output");
    if let Some(ctx) = ctx {
        ctx.emit_log(stream_name, message.into_owned());
    }
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

impl Drop for LxcContainer {
    fn drop(&mut self) {
        if !self.destroyed {
            warn!(container = %self.name, "lxc container was not destroyed before drop; cleanup may be needed");
        }
    }
}

fn redacted_command(command: &[String]) -> String {
    command
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let previous = index.checked_sub(1).and_then(|i| command.get(i));
            let sensitive = previous.is_some_and(|flag| {
                matches!(
                    flag.to_ascii_lowercase().as_str(),
                    "--token" | "--password" | "--secret" | "--api-key" | "-p"
                )
            });
            if sensitive {
                "<redacted>".to_owned()
            } else {
                shell_escape(value)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn lxc_command(program: &str) -> TokioCommand {
    let mut command = TokioCommand::new("sudo");
    command
        .arg("-n")
        .arg("--preserve-env=STATIX_AGENT_STATE_DIR,STATE_DIRECTORY")
        .arg(lxc_helper_path())
        .arg(program);
    if let Some(home) = lxc_process_home() {
        command.env("HOME", &home);
        command.env("XDG_CACHE_HOME", home.join(".cache"));
        command.env("XDG_CONFIG_HOME", home.join(".config"));
        command.env("XDG_DATA_HOME", home.join(".local").join("share"));
    }
    command.kill_on_drop(true);
    command
}

fn lxc_std_command(program: &str) -> StdCommand {
    let mut command = StdCommand::new("sudo");
    command
        .arg("-n")
        .arg("--preserve-env=STATIX_AGENT_STATE_DIR,STATE_DIRECTORY")
        .arg(lxc_helper_path())
        .arg(program);
    if let Some(home) = lxc_process_home() {
        command.env("HOME", &home);
        command.env("XDG_CACHE_HOME", home.join(".cache"));
        command.env("XDG_CONFIG_HOME", home.join(".config"));
        command.env("XDG_DATA_HOME", home.join(".local").join("share"));
    }
    command
}

fn lxc_helper_path() -> &'static str {
    "/usr/local/libexec/statix-agent-lxc"
}

fn lxc_process_home() -> Option<PathBuf> {
    env_path("STATIX_AGENT_STATE_DIR")
        .or_else(|| env_path("STATE_DIRECTORY"))
        .map(|path| path.join("lxc"))
}

fn lxc_storage_path() -> PathBuf {
    lxc_process_home()
        .map(|path| path.join("containers"))
        .unwrap_or_else(|| PathBuf::from("/var/lib/lxc"))
}

pub(crate) fn runtime_config_path(name: &str) -> PathBuf {
    lxc_storage_path().join(name).join("config")
}

pub(crate) fn runtime_log_path(name: &str) -> PathBuf {
    lxc_storage_path().join(name).join("statix-runtime.log")
}

pub(crate) fn runtime_command(program: &str, name: &str, args: &[String]) -> Vec<String> {
    let mut command = vec![
        "sudo".to_string(),
        "-n".to_string(),
        "--preserve-env=STATIX_AGENT_STATE_DIR,STATE_DIRECTORY".to_string(),
        lxc_helper_path().to_string(),
        program.to_string(),
        "-n".to_string(),
        name.to_string(),
        "-P".to_string(),
        lxc_storage_path().display().to_string(),
    ];
    command.extend(args.iter().cloned());
    command
}

pub(crate) async fn runtime_ipv4(name: &str) -> Result<Ipv4Addr> {
    let output = lxc_command("lxc-info")
        .arg("-n")
        .arg(name)
        .arg("-P")
        .arg(lxc_storage_path())
        .arg("-iH")
        .output()
        .await
        .with_context(|| format!("failed to inspect lxc runtime {name}"))?;
    if !output.status.success() {
        bail!(
            "failed to resolve IPv4 address for lxc runtime {name}: {}",
            summarize_raw_command_output(&output.stdout, &output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .find_map(|value| value.parse().ok())
        .ok_or_else(|| anyhow!("lxc runtime {name} has no IPv4 address"))
}

pub(crate) fn configure_runtime(name: &str, cpu: u32, memory_mb: u32) -> Result<()> {
    let mut config = fs::OpenOptions::new()
        .append(true)
        .open(runtime_config_path(name))
        .with_context(|| format!("failed to open lxc config for {name}"))?;
    write_job_lxc_config(
        &mut config,
        cpu.min(u8::MAX as u32) as u8,
        memory_mb,
        enforce_lxc_limits(),
    )?;
    Ok(())
}

fn ensure_lxc_directory_permissions() -> Result<()> {
    let Some(home) = lxc_process_home() else {
        return Ok(());
    };

    if let Some(state_dir) = home.parent() {
        set_traversable_directory(state_dir)?;
    }
    fs::create_dir_all(&home).with_context(|| format!("failed to create {}", home.display()))?;
    set_traversable_directory(&home)?;
    Ok(())
}

fn set_traversable_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata =
        fs::metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if !metadata.is_dir() {
        return Ok(());
    }

    let mut permissions = metadata.permissions();
    let mode = permissions.mode();
    let traversable_mode = mode | 0o711;
    if mode != traversable_mode {
        permissions.set_mode(traversable_mode);
        fs::set_permissions(path, permissions)
            .with_context(|| format!("failed to set permissions on {}", path.display()))?;
    }
    Ok(())
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn write_job_lxc_config(
    mut writer: impl Write,
    cpu: u8,
    memory_mb: u32,
    enforce_limits: bool,
) -> Result<()> {
    writeln!(writer, "\n# Statix job runtime config")?;
    if enforce_limits {
        let memory_bytes = u64::from(memory_mb) * 1024 * 1024;
        let cpu_quota = u64::from(cpu) * 100_000;
        writeln!(writer, "lxc.cgroup2.memory.max = {memory_bytes}")?;
        writeln!(writer, "lxc.cgroup2.cpu.max = {cpu_quota} 100000")?;
    } else {
        writeln!(
            writer,
            "# cgroup limits requested by Statix are not written because enforcement was disabled."
        )?;
    }
    writeln!(writer, "lxc.apparmor.profile = unconfined")?;
    writeln!(writer, "lxc.apparmor.allow_nesting = 1")?;
    writeln!(writer, "lxc.mount.auto = proc:rw sys:rw")?;
    Ok(())
}

fn enforce_lxc_limits() -> bool {
    env::var("STATIX_LXC_ENFORCE_LIMITS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::runtime_command;

    #[test]
    fn runtime_command_uses_privileged_helper_and_explicit_storage() {
        let command = runtime_command(
            "lxc-attach",
            "statix-project-runtime",
            &["--".into(), "true".into()],
        );

        assert_eq!(command[0], "sudo");
        assert_eq!(command[3], "/usr/local/libexec/statix-agent-lxc");
        assert_eq!(command[4], "lxc-attach");
        assert!(
            command
                .windows(2)
                .any(|pair| pair == ["-n", "statix-project-runtime"])
        );
        assert_eq!(command[7], "-P");
        assert!(command[8] == "/var/lib/lxc" || command[8].ends_with("/lxc/containers"));
        assert_eq!(&command[9..], ["--", "true"]);
    }
}
