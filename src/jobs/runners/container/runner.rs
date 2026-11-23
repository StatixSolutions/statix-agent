use std::{fs, future::Future, time::Instant};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use crate::{
    config::agent_state_dir,
    jobs::{CommandSpec, ExecutionContext, JobExecutionResult, PreparedWorkspace, Runner},
    logs,
};

use super::{
    archive::{WORKSPACE_ARCHIVE, create_workspace_archive},
    image::LxcImage,
    lxc::LxcContainer,
    shell::container_name,
};

const DEFAULT_CPU_COUNT: u8 = 2;
const DEFAULT_MEMORY_MB: u32 = 4096;

pub struct ContainerRunner {
    image: String,
    cpu: Option<u8>,
    memory_mb: Option<u32>,
}

impl ContainerRunner {
    pub fn new(image: String, cpu: Option<u8>, memory_mb: Option<u32>) -> Self {
        Self {
            image,
            cpu,
            memory_mb,
        }
    }
}

async fn lifecycle_step_with<T, F, O>(
    ctx: &ExecutionContext,
    container: &str,
    step: &str,
    operation: F,
    outcome: O,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
    O: Fn(&T) -> (String, Option<String>),
{
    let started = Instant::now();
    let _ = logs::append_lifecycle(
        "info",
        &ctx.log_scope,
        ctx.log_resource_id.as_deref(),
        Some(&ctx.job_id),
        Some(&ctx.attempt_id),
        step,
        "started",
        None,
        &format!("container={container}"),
    );
    let result = operation.await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match &result {
        Ok(value) => {
            let (step_outcome, detail) = outcome(value);
            let _ = logs::append_lifecycle(
                "info",
                &ctx.log_scope,
                ctx.log_resource_id.as_deref(),
                Some(&ctx.job_id),
                Some(&ctx.attempt_id),
                step,
                &step_outcome,
                Some(duration_ms),
                &format!(
                    "container={container}{}",
                    detail.map(|v| format!(": {v}")).unwrap_or_default()
                ),
            );
        }
        Err(error) => {
            let _ = logs::append_lifecycle(
                "error",
                &ctx.log_scope,
                ctx.log_resource_id.as_deref(),
                Some(&ctx.job_id),
                Some(&ctx.attempt_id),
                step,
                "failed",
                Some(duration_ms),
                &format!("container={container}: {error:#}"),
            );
        }
    }
    result
}

async fn lifecycle_step<T, F>(
    ctx: &ExecutionContext,
    container: &str,
    step: &str,
    operation: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    lifecycle_step_with(ctx, container, step, operation, |_| {
        ("succeeded".into(), None)
    })
    .await
}

async fn lifecycle_job_step<F>(
    ctx: &ExecutionContext,
    container: &str,
    step: &str,
    operation: F,
) -> Result<JobExecutionResult>
where
    F: Future<Output = Result<JobExecutionResult>>,
{
    lifecycle_step_with(ctx, container, step, operation, |result| {
        if result.status == "failed" {
            ("failed".into(), result.message.clone())
        } else {
            ("succeeded".into(), None)
        }
    })
    .await
}

async fn lifecycle_prepare_step<F>(
    ctx: &ExecutionContext,
    container: &str,
    step: &str,
    operation: F,
) -> Result<Option<JobExecutionResult>>
where
    F: Future<Output = Result<Option<JobExecutionResult>>>,
{
    lifecycle_step_with(ctx, container, step, operation, |result| match result {
        Some(job) if job.status == "failed" => ("failed".into(), job.message.clone()),
        _ => ("succeeded".into(), None),
    })
    .await
}

#[async_trait::async_trait]
impl Runner for ContainerRunner {
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

        let image = LxcImage::parse(&self.image)?;
        let container_name = container_name(&ctx.attempt_id);
        let cpu = self.cpu.unwrap_or(DEFAULT_CPU_COUNT);
        let memory_mb = self.memory_mb.unwrap_or(DEFAULT_MEMORY_MB);
        let runtime_root = agent_state_dir()?
            .join("container")
            .join(&ctx.job_id)
            .join(&ctx.attempt_id);
        lifecycle_step(ctx, &container_name, "prepare_runtime_root", async {
            fs::create_dir_all(&runtime_root)
                .with_context(|| format!("failed to create {}", runtime_root.display()))
        })
        .await?;

        info!(job_id = %ctx.job_id, container = %container_name, image = %self.image, "preparing privileged lxc container");
        let workspace_tar = runtime_root.join(WORKSPACE_ARCHIVE);
        lifecycle_step(
            ctx,
            &container_name,
            "archive_workspace",
            create_workspace_archive(&workspace_tar, &workspace.workdir),
        )
        .await?;

        let network = lifecycle_job_step(
            ctx,
            &container_name,
            "prepare_host_network",
            crate::jobs::execute(
                &crate::jobs::RunnerEnvironment::Host,
                ctx,
                workspace,
                &super::lxc::network_command(),
            ),
        )
        .await?;
        if network.status == "failed" {
            return Ok(network);
        }

        let mut container = lifecycle_step(
            ctx,
            &container_name,
            "create_lxc_container",
            LxcContainer::create(
                container_name.clone(),
                image.distribution.as_str(),
                image.release.as_str(),
                cpu,
                memory_mb,
            ),
        )
        .await?;

        let result = async {
            lifecycle_step(
                ctx,
                &container_name,
                "start_lxc_container",
                container.start(),
            )
            .await?;
            lifecycle_step(
                ctx,
                &container_name,
                "configure_guest_network",
                container.configure_guest_network(ctx.timeout_seconds),
            )
            .await?;
            lifecycle_step(
                ctx,
                &container_name,
                "configure_guest_dns",
                container.configure_guest_dns(ctx.timeout_seconds),
            )
            .await?;
            let prepared = lifecycle_prepare_step(
                ctx,
                &container_name,
                "provision_guest",
                container.prepare_guest(ctx, ctx.timeout_seconds, workspace),
            )
            .await?;
            if let Some(result) = prepared {
                return Ok(result);
            }
            lifecycle_step(
                ctx,
                &container_name,
                "copy_workspace_to_guest",
                container.copy_archive_to_guest(&workspace_tar),
            )
            .await?;
            lifecycle_job_step(
                ctx,
                &container_name,
                "run_container_command",
                container.run_command(ctx, ctx.timeout_seconds, command, workspace),
            )
            .await
        }
        .await;

        info!(job_id = %ctx.job_id, container = %container_name, "destroying lxc container");
        let cleanup = lifecycle_step(
            ctx,
            &container_name,
            "destroy_lxc_container",
            container.destroy(),
        )
        .await;
        if let Err(error) = cleanup {
            warn!(job_id = %ctx.job_id, container = %container_name, error = %error, "failed to destroy lxc container");
        }
        result
    }
}
