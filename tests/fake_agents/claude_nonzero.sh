#!/usr/bin/env bash
# A fake claude that emits an init event then crashes with a non-zero
# exit code and no terminal result.
set -euo pipefail

printf '{"type":"system","subtype":"init","session_id":"fake-crash","tools":[]}\n'
echo "boom: simulated crash" >&2
exit 7
