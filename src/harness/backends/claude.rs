//! The Claude Code backend (§57 of the confluence spec).
//!
//! Launches `claude -p <prompt> --output-format stream-json` with a
//! generated strict MCP config pointing at the confluence server, the
//! task token injected through the environment. Architecture agents
//! get read-only repository access and the Conseqa tools only; on
//! invalidation the harness cancels the child and never reuses the
//! conversation (§57.2).

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::confluence::AgentBackendMetadata;
use crate::harness::backend::{
    AgentBackend, AgentBackendError, AgentEvent, AgentEventSink, AgentExit, AgentHandle,
    AgentInvocation, AgentUsage, mcp_config_json,
};

use super::process::{LineParser, supervise};

const TOKEN_ENV_VAR: &str = "CONSEQA_TASK_TOKEN";

/// The Claude Code CLI adapter.
#[derive(Debug, Clone)]
pub struct ClaudeCliBackend {
    /// The `claude` executable (overridable for tests with a fake that
    /// emits the same stream-json shape, §105).
    program: String,

    /// Pass `--bare` (skip hooks, LSP, plugins). Appropriate when
    /// provider auth is configured for scripted use (§57).
    bare: bool,

    /// A specific model, when the run pins one.
    model: Option<String>,

    /// The tools the agent may use besides the Conseqa MCP tools.
    /// Architecture synthesis needs no filesystem write tools (§57).
    allowed_tools: Vec<String>,

    /// Extra flags appended verbatim, for forward compatibility.
    extra_args: Vec<String>,
}

impl Default for ClaudeCliBackend {
    fn default() -> Self {
        Self {
            program: "claude".to_string(),
            bare: false,
            model: None,
            // Read-only repository inspection plus the MCP tools. No
            // Write/Edit: architecture agents do not author source.
            allowed_tools: vec![
                "Read".to_string(),
                "Grep".to_string(),
                "Glob".to_string(),
                "mcp__conseqa".to_string(),
            ],
            extra_args: Vec::new(),
        }
    }
}

impl ClaudeCliBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Points the adapter at a specific executable — the seam the
    /// fake-CLI tests use.
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn bare(mut self, bare: bool) -> Self {
        self.bare = bare;
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    pub fn with_extra_args(mut self, args: Vec<String>) -> Self {
        self.extra_args = args;
        self
    }

    /// Writes the task's strict MCP config into the work directory and
    /// returns its path. The token is referenced as `${CONSEQA_TASK_TOKEN}`
    /// and supplied through the environment, never written to the file.
    fn write_mcp_config(&self, invocation: &AgentInvocation) -> Result<PathBuf, AgentBackendError> {
        std::fs::create_dir_all(&invocation.work_dir)?;

        let path = invocation
            .work_dir
            .join(format!("conseqa-mcp-{}.json", invocation.task.0));

        let config = mcp_config_json(&invocation.mcp_url, TOKEN_ENV_VAR);

        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("mcp config serializes"),
        )?;

        Ok(path)
    }

    fn build_args(&self, invocation: &AgentInvocation, mcp_config: &Path) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            invocation.prompt.clone(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            // stream-json in print mode requires --verbose.
            "--verbose".to_string(),
            "--mcp-config".to_string(),
            mcp_config.display().to_string(),
            // Only the confluence server; no user/project MCP servers
            // enter the architecture task (§57).
            "--strict-mcp-config".to_string(),
        ];

        if self.bare {
            args.push("--bare".to_string());
        }

        if !self.allowed_tools.is_empty() {
            args.push("--allowed-tools".to_string());
            args.extend(self.allowed_tools.iter().cloned());
        }

        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }

        if let Some(max_turns) = invocation.budget.max_turns {
            args.push("--max-turns".to_string());
            args.push(max_turns.to_string());
        }

        args.extend(self.extra_args.iter().cloned());

        args
    }
}

#[async_trait]
impl AgentBackend for ClaudeCliBackend {
    fn name(&self) -> &str {
        "claude-cli"
    }

    async fn run(
        &self,
        invocation: AgentInvocation,
        handle: AgentHandle,
        events: AgentEventSink,
    ) -> Result<AgentExit, AgentBackendError> {
        let mcp_config = self.write_mcp_config(&invocation)?;
        let args = self.build_args(&invocation, &mcp_config);

        let env = vec![(TOKEN_ENV_VAR.to_string(), invocation.task_token.clone())];

        let outcome = supervise(
            &self.program,
            &args,
            &env,
            invocation.repo.as_ref(),
            None,
            Box::new(ClaudeStreamParser::default()),
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
                name: "claude-cli".to_string(),
                version: None,
                session: outcome.session,
            },
        })
    }
}

/// Parses `claude --output-format stream-json`: `system/init` for the
/// session id and tool inventory, `assistant` messages for tool calls
/// and text, and the terminal `result` object for usage and outcome.
#[derive(Default)]
struct ClaudeStreamParser {
    session: Option<String>,
    final_message: Option<String>,
    usage: AgentUsage,
    errored: bool,
}

impl LineParser for ClaudeStreamParser {
    fn on_line(&mut self, line: &str, events: &AgentEventSink) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            // Non-JSON stderr-ish noise on stdout is surfaced as a log,
            // never fatal.
            let _ = events.send(AgentEvent::Log {
                message: line.to_string(),
            });

            return;
        };

        match value.get("type").and_then(|value| value.as_str()) {
            Some("system") => {
                if value.get("subtype").and_then(|value| value.as_str()) == Some("init") {
                    self.session = value
                        .get("session_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);

                    let _ = events.send(AgentEvent::SessionStarted {
                        session: self.session.clone(),
                    });
                }
            }

            Some("assistant") => {
                if let Some(content) = value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(|content| content.as_array())
                {
                    for item in content {
                        match item.get("type").and_then(|value| value.as_str()) {
                            Some("tool_use") => {
                                if let Some(name) =
                                    item.get("name").and_then(|value| value.as_str())
                                {
                                    let _ = events.send(AgentEvent::ToolCall {
                                        name: name.to_string(),
                                    });
                                }
                            }

                            Some("text") => {
                                if let Some(text) =
                                    item.get("text").and_then(|value| value.as_str())
                                {
                                    self.final_message = Some(text.to_string());
                                }
                            }

                            _ => {}
                        }
                    }
                }
            }

            Some("result") => {
                self.errored = value
                    .get("is_error")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);

                if let Some(result) = value.get("result").and_then(|value| value.as_str()) {
                    self.final_message = Some(result.to_string());
                }

                self.usage.turns = value.get("num_turns").and_then(|value| value.as_u64());

                self.usage.usd_cents = value
                    .get("total_cost_usd")
                    .and_then(|value| value.as_f64())
                    .map(|usd| (usd * 100.0).round() as u64);

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
                }

                let _ = events.send(AgentEvent::Usage {
                    tokens: self.usage.tokens,
                    usd_cents: self.usage.usd_cents,
                });
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
