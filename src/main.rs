mod config;
mod enrollment;
#[allow(dead_code)] // Disconnected pending real controllers/ops; see transport::dispatch.
mod jobs;
mod logs;
mod metrics;
mod system_info;
mod transport;
mod wireguard;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use config::{AgentConfig, agent_state_dir, resolve_login_config};
use enrollment::{LoginOptions, run_login};
use tokio::{
    process::{Child, Command as TokioCommand},
    select, signal,
    sync::watch,
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{EnvFilter, fmt};

use crate::wireguard::ensure_applied as ensure_wireguard_applied;

#[derive(Debug, Parser)]
#[command(name = "statix-agent", args_conflicts_with_subcommands = true)]
struct Cli {
    #[arg(long, exclusive = true)]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run(RunArgs),
    Login(LoginArgs),
    Update,
}

#[derive(Debug, Args, Default)]
struct RunArgs {
    /// Log every job the control plane sends without spinning up containers or VMs.
    #[arg(long)]
    debug_log_only: bool,
}

#[derive(Debug, Args)]
struct LoginArgs {
    #[arg(long = "api-base-url", value_name = "URL")]
    api_base_url: Option<String>,
    #[arg(long = "name", value_name = "NODE_NAME")]
    requested_name: Option<String>,
    #[arg(value_name = "URL", conflicts_with = "api_base_url")]
    api_base_url_positional: Option<String>,
}

impl LoginArgs {
    fn into_options(self) -> LoginOptions {
        LoginOptions {
            api_base_url: self.api_base_url.or(self.api_base_url_positional),
            requested_name: self.requested_name,
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if cli.version {
        println!("{}", format_version(&system_info::agent_version()));
        return;
    }

    init_logging();
    if let Err(error) = dispatch(cli).await {
        error!(error = %format_error_chain(&error), "fatal error");
        std::process::exit(1);
    }
}

fn format_version(version: &str) -> String {
    format!("statix-agent {version}")
}

fn init_logging() {
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).unwrap_or_else(|_| EnvFilter::new("info")),
        Err(_) if env_flag("STATIX_VERBOSE_LOGS") => EnvFilter::new("debug"),
        Err(_) => EnvFilter::new("info"),
    };
    fmt::Subscriber::builder()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(false)
        .with_writer(logs::agent_log_writer)
        .init();
    if let Err(error) = logs::append_agent("info", "statix-agent logging initialized") {
        eprintln!("failed to initialize local agent log spool: {error}");
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        None => run_agent(RunArgs::default()).await,
        Some(Command::Run(args)) => run_agent(args).await,
        Some(Command::Login(args)) => {
            let options = args.into_options();
            let login_config = resolve_login_config(options.api_base_url.clone());
            run_login(login_config, options).await
        }
        Some(Command::Update) => request_update().await,
    }
}

pub(crate) fn format_error_chain(error: &anyhow::Error) -> String {
    let mut parts = Vec::new();
    for cause in error.chain() {
        let text = cause.to_string();
        if parts.last() == Some(&text) {
            continue;
        }
        parts.push(text);
    }

    parts.join(": ")
}

async fn run_agent(args: RunArgs) -> Result<()> {
    let config = AgentConfig::load()?.context(
        "Agent identity not configured. Run `statix-agent login --api-base-url http://host:3001` with STATIX_AGENT_CONFIG pointing at the service config, or set NODE_ID/NODE_TOKEN in the environment.",
    )?;
    info!(node_id = %config.node_id, debug_log_only = args.debug_log_only, "starting agent");
    debug!(state_dir = %agent_state_dir()?.display(), "resolved agent state directory");
    debug!(websocket_url = %redact_url(&config.agent_ws_url), api_url = %redact_url(&config.api_base_url), publish_interval_ms = config.publish_interval_ms, system_info_check_interval_ms = config.system_info_check_interval_ms, "loaded runtime configuration");

    if let Some(wireguard) = config
        .wireguard
        .as_ref()
        .filter(|_| env_flag("STATIX_APPLY_WIREGUARD"))
    {
        match ensure_wireguard_applied(wireguard).await {
            Ok(path) => {
                info!(interface = %wireguard.interface_name, config_path = %path.display(), "wireguard configuration applied");
            }
            Err(error) => {
                warn!(error = %error, "wireguard apply failed");
            }
        }
    }

    let (stop_tx, stop_rx) = watch::channel(false);
    tokio::spawn(shutdown_signal_task(stop_tx));

    transport::session::run(&config, stop_rx, args.debug_log_only).await?;

    info!("agent stopped");
    Ok(())
}

async fn shutdown_signal_task(stop_tx: watch::Sender<bool>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            select! {
                _ = signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        } else {
            let _ = signal::ctrl_c().await;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = signal::ctrl_c().await;
    }

    let _ = stop_tx.send(true);
}

pub(crate) fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

pub(crate) fn redact_url(value: &str) -> String {
    value
        .split_once("://")
        .map(|(scheme, rest)| {
            let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
            let host = authority.rsplit('@').next().unwrap_or(authority);
            format!("{scheme}://{host}")
        })
        .unwrap_or_else(|| value.to_owned())
}

async fn request_update() -> Result<()> {
    let mut runner = RealUpdateCommandRunner;
    let mut log_stream = start_update_log_stream();
    let result = request_update_with(&mut runner).await;

    if let Some(child) = log_stream.as_mut() {
        if let Err(error) = child.kill().await {
            debug!(error = %error, "failed to stop update journal stream");
        }
        let _ = child.wait().await;
    }

    result
}

fn start_update_log_stream() -> Option<Child> {
    #[cfg(not(target_os = "linux"))]
    {
        None
    }

    #[cfg(target_os = "linux")]
    {
        let service = std::env::var("STATIX_UPDATE_SERVICE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "statix-agent-update.service".to_owned());

        match TokioCommand::new("journalctl")
            .args([
                "--follow",
                "--unit",
                service.as_str(),
                "--since",
                "now",
                "--no-pager",
            ])
            .spawn()
        {
            Ok(child) => Some(child),
            Err(error) => {
                warn!(error = %error, "failed to stream update journal; continuing without live logs");
                None
            }
        }
    }
}

#[async_trait::async_trait]
trait UpdateCommandRunner: Send {
    async fn output(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<std::process::Output>;
}

struct RealUpdateCommandRunner;

#[async_trait::async_trait]
impl UpdateCommandRunner for RealUpdateCommandRunner {
    async fn output(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> std::io::Result<std::process::Output> {
        TokioCommand::new(program).args(args).output().await
    }
}

async fn request_update_with(runner: &mut dyn UpdateCommandRunner) -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        bail!("agent update requests are supported on Linux only");
    }

    #[cfg(target_os = "linux")]
    {
        let service = std::env::var("STATIX_UPDATE_SERVICE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "statix-agent-update.service".to_owned());
        let output = match runner
            .output("systemctl", &["start", service.as_str()])
            .await
        {
            Ok(output) if output.status.success() => output,
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
                if stderr.contains("interactive authentication required")
                    || stderr.contains("access denied")
                    || stderr.contains("permission denied")
                {
                    runner
                        .output("sudo", &["-n", "systemctl", "start", service.as_str()])
                        .await
                        .context("failed to start update service via sudo")?
                } else {
                    output
                }
            }
            Err(error) => runner
                .output("sudo", &["-n", "systemctl", "start", service.as_str()])
                .await
                .with_context(|| format!("failed to start update service: {error}"))?,
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            bail!(
                "systemctl start {service} failed: {}",
                if stderr.is_empty() {
                    "unknown error"
                } else {
                    stderr.as_str()
                }
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, os::unix::process::ExitStatusExt, process::Output};

    use super::*;

    struct FakeUpdateCommandRunner {
        calls: Vec<(String, Vec<String>)>,
        responses: VecDeque<std::io::Result<Output>>,
    }

    impl FakeUpdateCommandRunner {
        fn new(responses: Vec<std::io::Result<Output>>) -> Self {
            Self {
                calls: Vec::new(),
                responses: responses.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl UpdateCommandRunner for FakeUpdateCommandRunner {
        async fn output(&mut self, program: &str, args: &[&str]) -> std::io::Result<Output> {
            self.calls.push((
                program.to_string(),
                args.iter().map(|value| (*value).to_string()).collect(),
            ));
            self.responses.pop_front().unwrap_or_else(|| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected update command invocation",
                ))
            })
        }
    }

    fn success_output() -> Output {
        Output {
            status: ExitStatusExt::from_raw(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn permission_denied_output() -> Output {
        Output {
            status: ExitStatusExt::from_raw(1),
            stdout: Vec::new(),
            stderr: b"Permission denied".to_vec(),
        }
    }

    #[tokio::test]
    async fn request_update_falls_back_to_sudo_when_systemctl_is_denied() {
        let mut runner = FakeUpdateCommandRunner::new(vec![
            Ok(permission_denied_output()),
            Ok(success_output()),
        ]);

        request_update_with(&mut runner).await.unwrap();

        assert_eq!(
            runner.calls,
            vec![
                (
                    "systemctl".to_string(),
                    vec![
                        "start".to_string(),
                        "statix-agent-update.service".to_string()
                    ]
                ),
                (
                    "sudo".to_string(),
                    vec![
                        "-n".to_string(),
                        "systemctl".to_string(),
                        "start".to_string(),
                        "statix-agent-update.service".to_string()
                    ]
                ),
            ]
        );
    }

    #[test]
    fn cli_parses_update_command() {
        assert!(matches!(
            Cli::try_parse_from(["statix-agent", "update"])
                .unwrap()
                .command,
            Some(Command::Update)
        ));
    }

    #[test]
    fn cli_parses_run_debug_log_only_flag() {
        let cli = Cli::try_parse_from(["statix-agent", "run", "--debug-log-only"]).unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Run(RunArgs {
                debug_log_only: true
            }))
        ));
    }

    #[test]
    fn cli_run_without_flag_defaults_debug_log_only_to_false() {
        let cli = Cli::try_parse_from(["statix-agent", "run"]).unwrap();

        assert!(matches!(
            cli.command,
            Some(Command::Run(RunArgs {
                debug_log_only: false
            }))
        ));
    }

    #[test]
    fn cli_parses_version_flag() {
        let cli = Cli::try_parse_from(["statix-agent", "--version"]).unwrap();

        assert!(cli.version);
        assert!(cli.command.is_none());
        assert_eq!(format_version("v1.2.3"), "statix-agent v1.2.3");
    }

    #[test]
    fn cli_rejects_version_with_a_subcommand() {
        assert!(Cli::try_parse_from(["statix-agent", "--version", "update"]).is_err());
    }

    #[test]
    fn redact_url_removes_path_and_credentials() {
        assert_eq!(
            redact_url("https://user:secret@example.test/api?token=abc"),
            "https://example.test"
        );
        assert_eq!(
            redact_url("http://localhost:3001/api"),
            "http://localhost:3001"
        );
    }
}
