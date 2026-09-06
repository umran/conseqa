#!/usr/bin/env bash
# A fake `claude -p --output-format stream-json` that emits the real
# event shape and exits 0. It echoes back facts the adapter test
# asserts on: the MCP config path it was handed, and the task token
# from the environment (so the test confirms env injection, not the
# command line).

set -euo pipefail

mcp_config=""
prompt=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mcp-config) mcp_config="$2"; shift 2 ;;
    -p) prompt="$2"; shift 2 ;;
    *) shift ;;
  esac
done

session="fake-session-0001"
token="${CONSEQA_TASK_TOKEN:-<unset>}"

printf '{"type":"system","subtype":"init","session_id":"%s","tools":["Read","mcp__conseqa"],"mcp_servers":[{"name":"conseqa"}]}\n' "$session"
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__conseqa__task_context"}]},"session_id":"%s"}\n' "$session"
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__conseqa__submit_patch"}]},"session_id":"%s"}\n' "$session"
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working"}]},"session_id":"%s"}\n' "$session"
printf '{"type":"result","subtype":"success","is_error":false,"num_turns":3,"total_cost_usd":0.0123,"session_id":"%s","usage":{"input_tokens":100,"output_tokens":50},"result":"config=%s token=%s"}\n' "$session" "$mcp_config" "$token"
