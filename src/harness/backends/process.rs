//! Shared subprocess supervision: spawn a provider CLI, stream its
//! stdout line by line through a parser, and honor cancellation and
//! wall-clock timeout.
//!
//! Both CLI backends emit newline-delimited JSON events, so the
//! line-parsing loop is common; only the command line and the
//! per-line interpretation differ.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::harness::backend::{
    AgentBackendError, AgentEvent, AgentEventSink, AgentExitStatus, AgentHandle, AgentUsage,
};

/// A parser turning one CLI event line into harness events and,
/// eventually, the session's terminal facts.
pub trait LineParser: Send {
    /// Interprets one line, forwarding any [`AgentEvent`]s. Terminal
    /// facts (session id, usage, final message, error) accumulate in
    /// the parser's own state.
    fn on_line(&mut self, line: &str, events: &AgentEventSink);

    fn session(&self) -> Option<String>;
    fn usage(&self) -> AgentUsage;
    fn final_message(&self) -> Option<String>;

    /// Whether the stream reported a semantic error even if the
    /// process exits zero.
    fn errored(&self) -> bool;
}

/// A launched process's outcome, before the backend layers its own
/// metadata on top.
pub struct ProcessOutcome {
    pub status: AgentExitStatus,
    pub session: Option<String>,
    pub final_message: Option<String>,
    pub usage: AgentUsage,
}

/// Spawns `program args…`, streams stdout through `parser`, and
/// resolves when the process exits, is cancelled, or times out.
///
/// `env` entries are set on the child (the task token lives here, not
/// on the command line, §57.1). `cwd`, when set, is the read-only
/// application repository.
#[allow(clippy::too_many_arguments)]
pub async fn supervise(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&PathBuf>,
    stdin_data: Option<String>,
    mut parser: Box<dyn LineParser>,
    handle: &AgentHandle,
    events: &AgentEventSink,
    timeout_secs: Option<u64>,
) -> Result<ProcessOutcome, AgentBackendError> {
    let mut command = Command::new(program);

    command
        .args(args)
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for (key, value) in env {
        command.env(key, value);
    }

    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .map_err(|error| AgentBackendError::Launch(format!("{program}: {error}")))?;

    if let Some(data) = stdin_data
        && let Some(mut stdin) = child.stdin.take()
    {
        use tokio::io::AsyncWriteExt;

        stdin.write_all(data.as_bytes()).await?;
        stdin.shutdown().await?;
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentBackendError::Stream("child stdout was not captured".to_string()))?;

    let stderr = child.stderr.take();

    // Drain stderr into log events without blocking the main loop.
    if let Some(stderr) = stderr {
        let events = events.clone();

        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let _ = events.send(AgentEvent::Log { message: line });
            }
        });
    }

    let mut lines = BufReader::new(stdout).lines();

    let timeout = timeout_secs.map(std::time::Duration::from_secs);
    let deadline = timeout.map(|duration| tokio::time::Instant::now() + duration);

    let status = loop {
        let read_line = lines.next_line();

        tokio::select! {
            biased;

            () = handle.cancel.cancelled() => {
                let _ = child.start_kill();
                let _ = child.wait().await;

                break AgentExitStatus::Cancelled;
            }

            () = sleep_until(deadline) => {
                let _ = child.start_kill();
                let _ = child.wait().await;

                break AgentExitStatus::TimedOut;
            }

            line = read_line => {
                match line {
                    Ok(Some(line)) => {
                        if !line.trim().is_empty() {
                            parser.on_line(&line, events);
                        }
                    }

                    Ok(None) => {
                        // Stream closed; reap the process for its code.
                        let exit = child.wait().await?;

                        break if exit.success() && !parser.errored() {
                            AgentExitStatus::Completed
                        } else {
                            AgentExitStatus::Failed { code: exit.code() }
                        };
                    }

                    Err(error) => {
                        let _ = child.start_kill();
                        let _ = child.wait().await;

                        return Err(AgentBackendError::Io(error));
                    }
                }
            }
        }
    };

    Ok(ProcessOutcome {
        status,
        session: parser.session(),
        final_message: parser.final_message(),
        usage: parser.usage(),
    })
}

/// Sleeps until `deadline`, or forever when there is none, so the
/// timeout arm of the select can always be written.
async fn sleep_until(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}
