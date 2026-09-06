#!/usr/bin/env bash
# A fake claude that starts a session then hangs, so the harness must
# cancel or time it out. Sleeps in short increments so kill-on-drop and
# start_kill terminate it promptly.
set -euo pipefail

printf '{"type":"system","subtype":"init","session_id":"fake-hang","tools":[]}\n'

for _ in $(seq 1 600); do
  sleep 0.1
done
