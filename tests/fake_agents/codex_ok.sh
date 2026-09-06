#!/usr/bin/env bash
# A fake `codex exec --json` emitting the documented JSONL event
# vocabulary and exiting 0. Echoes the config overrides it received and
# the token from the environment.
set -euo pipefail

overrides=""
prompt=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -c) overrides="$overrides;$2"; shift 2 ;;
    exec|--json|--ephemeral|--sandbox|read-only) shift ;;
    *) prompt="$1"; shift ;;
  esac
done

token="${CONSEQA_TASK_TOKEN:-<unset>}"

# Strip double quotes so the echoed override values stay valid JSON
# inside the event line; the real Codex emits properly escaped JSON.
overrides="${overrides//\"/}"

printf '{"type":"thread.started","thread_id":"codex-thread-1"}\n'
printf '{"type":"turn.started"}\n'
printf '{"type":"item.completed","item":{"type":"mcp_tool_call","tool":"conseqa.task_context"}}\n'
printf '{"type":"item.completed","item":{"type":"agent_message","text":"overrides=%s token=%s"}}\n' "$overrides" "$token"
printf '{"type":"turn.completed","usage":{"input_tokens":80,"output_tokens":40}}\n'
