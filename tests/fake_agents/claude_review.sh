#!/usr/bin/env bash
# A fake `claude -p --output-format stream-json` reviewer. It reads the
# declaration and the record digest its prompt names, and reports a
# confirming review through the MCP server its strict config names,
# with the token from the environment: what a real agent does through
# its `record_review` tool.

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

url=$(sed -n 's/.*"url": *"\([^"]*\)".*/\1/p' "$mcp_config" | head -n 1)
key=$(printf '%s\n' "$prompt" | sed -n 's/^- declaration: "\(.*\)"$/\1/p' | head -n 1)
digest=$(printf '%s\n' "$prompt" | sed -n 's/^- record: "\(.*\)"$/\1/p' | head -n 1)

headers=$(mktemp)
session=""

post() {
  local extra=()

  if [[ -n "$session" ]]; then
    extra=(-H "Mcp-Session-Id: $session")
  fi

  curl -sS -D "$headers" -o /dev/null -X POST "$url" \
    -H 'Content-Type: application/json' \
    -H 'Accept: application/json, text/event-stream' \
    -H "Authorization: Bearer ${CONSEQA_TASK_TOKEN}" \
    ${extra[@]+"${extra[@]}"} \
    --data "$1"
}

post '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fake-reviewer","version":"0"}}}'
session=$(grep -i '^mcp-session-id:' "$headers" | head -n 1 | cut -d' ' -f2 | tr -d '\r')

post '{"jsonrpc":"2.0","method":"notifications/initialized"}'
post "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"record_review\",\"arguments\":{\"declaration\":\"$key\",\"record\":\"$digest\",\"verdict\":\"confirmed\",\"items\":[]}}}"

rm -f "$headers"

agent="fake-review-0001"

printf '{"type":"system","subtype":"init","session_id":"%s","tools":["Read","mcp__conseqa"],"mcp_servers":[{"name":"conseqa"}]}\n' "$agent"
printf '{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__conseqa__record_review"}]},"session_id":"%s"}\n' "$agent"
printf '{"type":"result","subtype":"success","is_error":false,"num_turns":2,"total_cost_usd":0.001,"session_id":"%s","usage":{"input_tokens":10,"output_tokens":5},"result":"reviewed %s"}\n' "$agent" "$key"
