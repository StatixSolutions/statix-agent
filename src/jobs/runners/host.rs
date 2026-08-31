use std::process::Stdio;

use anyhow::{Result, anyhow, bail};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command as TokioCommand,
    time::Duration,
};
use tracing::{debug, info, warn};

use crate::jobs::{
    CommandSpec, ExecutionContext, JobExecutionResult, PreparedWorkspace, Runner,
    summarize_command_output,
};

pub struct HostRunner;

#[async_trait::async_trait]
impl Runner for HostRunner {
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

        let mut process = TokioCommand::new(&command.argv[0]);
        process.args(&command.argv[1..]);
        process.current_dir(
            command
                .cwd
                .as_deref()
                .unwrap_or(workspace.workdir.to_str().unwrap_or(".")),
        );
        process.envs(&command.env);
        process.stdout(Stdio::piped());
        process.stderr(Stdio::piped());
        process.kill_on_drop(true);

        debug!(
            job_id = %ctx.job_id,
            attempt_id = %ctx.attempt_id,
            command = %command.argv[0],
            argument_count = command.argv.len().saturating_sub(1),
            cwd = %command.cwd.as_deref().unwrap_or(workspace.workdir.to_str().unwrap_or(".")),
            timeout_seconds = ctx.timeout_seconds,
            "starting host command"
        );

        let mut child = process.spawn().map_err(anyhow::Error::from).map_err(|error| {
            ctx.emit_log(crate::jobs::JobLogStream::Stderr, error.to_string());
            tracing::error!(job_id = %ctx.job_id, attempt_id = %ctx.attempt_id, command = %command.argv[0], error = %error, "failed to spawn host command");
            error.context(format!(
                "failed to run {} in {} for attempt {}",
                command.argv[0], workspace.workdir.display(), ctx.attempt_id
            ))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            tracing::error!(job_id = %ctx.job_id, attempt_id = %ctx.attempt_id, "host command stdout pipe was unavailable");
            anyhow!("failed to capture command stdout for job {} attempt {}", ctx.job_id, ctx.attempt_id)
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            tracing::error!(job_id = %ctx.job_id, attempt_id = %ctx.attempt_id, "host command stderr pipe was unavailable");
            anyhow!("failed to capture command stderr for job {} attempt {}", ctx.job_id, ctx.attempt_id)
        })?;
        let stdout_ctx = ctx.clone();
        let stderr_ctx = ctx.clone();
        let result = tokio::time::timeout(Duration::from_secs(ctx.timeout_seconds), async move {
            let stdout_task = read_stream(
                BufReader::new(stdout),
                stdout_ctx,
                crate::jobs::JobLogStream::Stdout,
            );
            let stderr_task = read_stream(
                BufReader::new(stderr),
                stderr_ctx,
                crate::jobs::JobLogStream::Stderr,
            );
            let wait = child.wait();
            let (stdout, stderr, status) = tokio::join!(stdout_task, stderr_task, wait);
            Ok::<_, anyhow::Error>((stdout?, stderr?, status?))
        })
        .await;
        let (stdout, stderr, status) = match result {
            Ok(result) => result.map_err(|error| anyhow!("command execution failed: {error}"))?,
            Err(_) => {
                let message = format!("command timed out after {} seconds", ctx.timeout_seconds);
                ctx.emit_log(crate::jobs::JobLogStream::Stderr, message.clone());
                warn!(job_id = %ctx.job_id, attempt_id = %ctx.attempt_id, timeout_seconds = ctx.timeout_seconds, "host command timed out");
                return Err(anyhow!(message));
            }
        };

        debug!(job_id = %ctx.job_id, attempt_id = %ctx.attempt_id, command = %command.argv[0], "host command completed");

        let message = summarize_command_output(&workspace.workdir, &stdout, &stderr);
        if status.success() {
            info!(job_id = %ctx.job_id, status = "succeeded", "host command finished");
            Ok(JobExecutionResult {
                status: "succeeded",
                message: Some(message),
            })
        } else {
            warn!(job_id = %ctx.job_id, status = "failed", exit_status = %status, "host command failed");
            Ok(JobExecutionResult {
                status: "failed",
                message: Some(message),
            })
        }
    }
}

async fn read_stream<R>(
    mut reader: BufReader<R>,
    ctx: ExecutionContext,
    stream: crate::jobs::JobLogStream,
) -> Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = Vec::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            break;
        }
        let value = line.trim_end_matches(['\r', '\n']).to_owned();
        ctx.emit_log(stream, value.clone());
        lines.push(value);
    }
    Ok(lines.join("\n").into_bytes())
}
