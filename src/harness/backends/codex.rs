//! The Codex backend (§58 of the confluence spec).
//!
//! Launches `codex exec --json --ephemeral --sandbox read-only` with
//! one-shot `-c` config overrides pointing its `conseqa` MCP server at
//! the confluence endpoint, the task token supplied through a
//! per-server environment variable. CLI overrides are used rather than
//! editing the user's repository. On invalidation the harness
//! terminates the process and spawns a fresh session (§58.1).
//!
//! Codex is not exercised in default CI; the JSONL parser follows the
//! documented event vocabulary — `thread.started`, `turn.started`,
//! `item.*`, `turn.completed`, `turn.failed`, `error` — and stays
//! tolerant of unknown shapes.

use async_trait::async_trait;

use crate::confluence::AgentBackendMetadata;
use crate::harness::backend::{
    AgentBackend, AgentBackendError, AgentEvent, AgentEventSink, AgentExit, AgentHandle,
    AgentInvocation, AgentUsage,
};

use super::process::{LineParser, supervise};

const TOKEN_ENV_VAR: &str = "CONSEQA_TASK_TOKEN";

/// The Codex CLI adapter.
#[derive(Debug, Clone)]
pub struct CodexCliBackend {
    program: String,
    model: Option<String>,
    extra_args: Vec<String>,
}

impl Default for CodexCliBackend {
    fn default() -> Self {
        Self {
            program: "codex".to_string(),
            model: None,
            extra_args: Vec::new(),
        }
    }
}

impl CodexCliBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    fn build_args(&self, invocation: &AgentInvocation) -> Vec<String> {
        let mut args = vec![
            "exec".to_string(),
            "--json".to_string(),
            "--ephemeral".to_string(),
            // Architecture synthesis reads the repository, never writes
            // it.
            "--sandbox".to_string(),
            "read-only".to_string(),
            "-c".to_string(),
            format!("mcp_servers.conseqa.url=\"{}\"", invocation.mcp_url),
            "-c".to_string(),
            format!("mcp_servers.conseqa.bearer_token_env_var=\"{TOKEN_ENV_VAR}\""),
            "-c".to_string(),
            "mcp_servers.conseqa.required=true".to_string(),
        ];

        if let Some(model) = &self.model {
            args.push("-c".to_string());
            args.push(format!("model=\"{model}\""));
        }

        args.extend(self.extra_args.iter().cloned());

        // The prompt is the trailing positional argument.
        args.push(invocation.prompt.clone());

        args
    }
}

#[async_trait]
impl AgentBackend for CodexCliBackend {
    fn name(&self) -> &str {
        "codex-cli"
    }

    async fn run(
        &self,
        invocation: AgentInvocation,
        handle: AgentHandle,
        events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        let args = self.build_args(&invocation);
        let env = vec![(TOKEN_ENV_VAR.to_string(), invocation.task_token.clone())];

        let outcome = supervise(
            &self.program,
            &args,
            &env,
            invocation.repo.as_ref(),
            None,
            Box::new(CodexStreamParser::default()),
            &handle,
            &events,
            invocation.budget.max_wall_time_secs,
        )
        .await?;

        Ok(AgentExit {
            status: outcome.status,
            session: outcome.session.clone(),
            final_message: outcome.final_message,
            usage: outcome.usage,
            backend: AgentBackendMetadata {
                name: "codex-cli".to_string(),
                version: None,
                session: outcome.session,
            },
        })
    }
}

/// Parses `codex exec --json` JSONL. Codex frames events under a
/// `type` discriminator; this reads `thread.started` for the session
/// id, `item.*` for tool calls and text, and `turn.failed`/`error` for
/// semantic failure.
#[derive(Default)]
struct CodexStreamParser {
    session: Option<String>,
    final_message: Option<String>,
    usage: AgentUsage,
    errored: bool,
}

impl LineParser for CodexStreamParser {
    fn on_line(&mut self, line: &str, events: &AgentEventSink) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            let _ = events.send(AgentEvent::Log {
                message: line.to_string(),
            });

            return;
        };

        let kind = value.get("type").and_then(|value| value.as_str());

        match kind {
            Some("thread.started") => {
                self.session = value
                    .get("thread_id")
                    .or_else(|| value.get("thread").and_then(|thread| thread.get("id")))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);

                let _ = events.send(AgentEvent::SessionStarted {
                    session: self.session.clone(),
                });
            }

            Some("error") | Some("turn.failed") => {
                self.errored = true;

                let message = value
                    .get("error")
                    .and_then(|error| error.get("message").or(Some(error)))
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| value.to_string());

                let _ = events.send(AgentEvent::Log { message });
            }

            Some("turn.completed") => {
                if let Some(usage) = value.get("usage") {
                    let input = usage
                        .get("input_tokens")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);

                    let output = usage
                        .get("output_tokens")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);

                    self.usage.tokens = Some(input + output);

                    let _ = events.send(AgentEvent::Usage {
                        tokens: self.usage.tokens,
                        usd_cents: None,
                    });
                }
            }

            Some(kind) if kind.starts_with("item.") => {
                self.on_item(value.get("item").unwrap_or(&value), events);
            }

            _ => {}
        }
    }

    fn session(&self) -> Option<String> {
        self.session.clone()
    }

    fn usage(&self) -> AgentUsage {
        self.usage
    }

    fn final_message(&self) -> Option<String> {
        self.final_message.clone()
    }

    fn errored(&self) -> bool {
        self.errored
    }
}

impl CodexStreamParser {
    fn on_item(&mut self, item: &serde_json::Value, events: &AgentEventSink) {
        match item.get("type").and_then(|value| value.as_str()) {
            Some("mcp_tool_call") | Some("tool_call") | Some("command_execution") => {
                let name = item
                    .get("tool")
                    .or_else(|| item.get("name"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("tool")
                    .to_string();

                let _ = events.send(AgentEvent::ToolCall { name });
            }

            Some("agent_message") | Some("assistant_message") => {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(|value| value.as_str())
                {
                    self.final_message = Some(text.to_string());
                }
            }

            _ => {}
        }
    }
}
