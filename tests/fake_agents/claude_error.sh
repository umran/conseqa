#!/usr/bin/env bash
# A fake claude that reports a semantic error in its terminal result
# while exiting 0 — the adapter must treat is_error:true as a failure.
set -euo pipefail

session="fake-session-err"
printf '{"type":"system","subtype":"init","session_id":"%s","tools":[]}\n' "$session"
printf '{"type":"result","subtype":"success","is_error":true,"num_turns":1,"total_cost_usd":0,"session_id":"%s","usage":{"input_tokens":10,"output_tokens":0},"result":"Not logged in"}\n' "$session"
