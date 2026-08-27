pub mod runners;

use std::path::{Path, PathBuf};

use anyhow::Result;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum RunnerEnvironment {
    Microvm {
        image: String,
        cpu: Option<u8>,
        memory_mb: Option<u32>,
    },
    ProjectMicrovm {
        project_id: String,
        environment: String,
        image: String,
        cpu: Option<u8>,
        memory_mb: Option<u32>,
    },
    Container {
        image: String,
        cpu: Option<u8>,
        memory_mb: Option<u32>,
    },
}

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

impl CommandSpec {
    pub fn new(argv: &[String]) -> Result<Self> {
        if argv.is_empty() || argv[0].trim().is_empty() {
            anyhow::bail!("command must contain at least one non-empty token");
        }
        Ok(Self {
            argv: argv.to_vec(),
            env: BTreeMap::new(),
            cwd: None,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.argv.is_empty() || self.argv[0].trim().is_empty() {
            anyhow::bail!("command must contain at least one non-empty token");
        }
        for key in self.env.keys() {
            let mut chars = key.chars();
            let valid = chars
                .next()
                .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
                && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());
            if !valid {
                anyhow::bail!("invalid environment variable name: {key}");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CommandSpec;

    #[test]
    fn command_spec_rejects_empty_argv() {
        assert!(CommandSpec::new(&[]).is_err());
        assert!(CommandSpec::new(&[String::new()]).is_err());
    }

    #[test]
    fn command_spec_preserves_argv_and_defaults() {
        let argv = vec![
            "printf".to_string(),
            "%s".to_string(),
            "hello world".to_string(),
        ];
        let command = CommandSpec::new(&argv).unwrap();
        assert_eq!(command.argv, argv);
        assert!(command.env.is_empty());
        assert!(command.cwd.is_none());
    }

    #[test]
    fn command_spec_rejects_invalid_environment_names() {
        let mut command = CommandSpec::new(&["true".to_string()]).unwrap();
        command
            .env
            .insert("bad-name".to_string(), "value".to_string());
        assert!(command.validate().is_err());
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionContext {
    pub job_id: String,
    pub attempt_id: String,
    pub timeout_seconds: u64,
    pub log_tx: Option<mpsc::UnboundedSender<JobLogLine>>,
}

impl ExecutionContext {
    pub(crate) fn emit_log(&self, stream: JobLogStream, line: impl Into<String>) {
        let Some(log_tx) = &self.log_tx else {
            return;
        };

        let _ = log_tx.send(JobLogLine {
            job_id: self.job_id.clone(),
            attempt_id: self.attempt_id.clone(),
            stream,
            line: line.into(),
        });
    }
}

#[derive(Debug, Clone)]
pub struct JobLogLine {
    pub job_id: String,
    pub attempt_id: String,
    pub stream: JobLogStream,
    pub line: String,
}

#[derive(Debug, Clone, Copy)]
pub enum JobLogStream {
    Stdout,
    Stderr,
}

impl JobLogStream {
    pub fn as_str(self) -> &'static str {
        match self {
            JobLogStream::Stdout => "stdout",
            JobLogStream::Stderr => "stderr",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PreparedWorkspace {
    pub workdir: PathBuf,
}

pub struct JobExecutionResult {
    pub status: &'static str,
    pub message: Option<String>,
}

pub async fn execute(
    environment: &RunnerEnvironment,
    ctx: &ExecutionContext,
    workspace: &PreparedWorkspace,
    command: &[String],
) -> Result<JobExecutionResult> {
    execute_spec(environment, ctx, workspace, CommandSpec::new(command)?).await
}

pub async fn execute_spec(
    environment: &RunnerEnvironment,
    ctx: &ExecutionContext,
    workspace: &PreparedWorkspace,
    command: CommandSpec,
) -> Result<JobExecutionResult> {
    command.validate()?;
    match environment {
        RunnerEnvironment::Microvm {
            image,
            cpu,
            memory_mb,
        } => {
            runners::microvm::MicrovmRunner::new(image.clone(), *cpu, *memory_mb)
                .execute(ctx, workspace, &command)
                .await
        }
        RunnerEnvironment::ProjectMicrovm {
            project_id,
            environment,
            image,
            cpu,
            memory_mb,
        } => {
            runners::microvm::ProjectMicrovmRunner::new(
                project_id.clone(),
                environment.clone(),
                image.clone(),
                *cpu,
                *memory_mb,
            )
            .execute(ctx, workspace, &command)
            .await
        }
        RunnerEnvironment::Container {
            image,
            cpu,
            memory_mb,
        } => {
            runners::container::ContainerRunner::new(image.clone(), *cpu, *memory_mb)
                .execute(ctx, workspace, &command)
                .await
        }
    }
}

#[async_trait::async_trait]
pub(crate) trait Runner {
    async fn execute(
        &self,
        ctx: &ExecutionContext,
        workspace: &PreparedWorkspace,
        command: &CommandSpec,
    ) -> Result<JobExecutionResult>;
}

pub(crate) fn summarize_command_output(cwd: &Path, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);

    if stderr.trim().is_empty() {
        if stdout.trim().is_empty() {
            format!("{}: command completed with no output", cwd.display())
        } else {
            format!("{}: {}", cwd.display(), truncate_output(&stdout))
        }
    } else if stdout.trim().is_empty() {
        format!("{}: {}", cwd.display(), truncate_output(&stderr))
    } else {
        format!(
            "{}: stdout:\n{}\n\nstderr:\n{}",
            cwd.display(),
            truncate_output(&stdout),
            truncate_output(&stderr)
        )
    }
}

fn truncate_output(value: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let trimmed = value.trim();
    if trimmed.chars().count() <= MAX_CHARS {
        return trimmed.to_owned();
    }

    let truncated = trimmed.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}
