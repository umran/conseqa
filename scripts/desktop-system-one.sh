#!/usr/bin/env bash
# Turns System One (hosted Jev) on for the `conseqa-confluence stdio`
# server Claude Desktop runs, without touching Desktop's own config —
# Desktop rewrites that file from memory while it runs, so edits made
# there do not stick. Instead this writes two files only you own:
#
#   ~/.conseqa/typesafe-key   the key, mode 600 (prompted, not echoed)
#   ~/.conseqa/stdio.args     flags `stdio` appends to its command line
#
#   scripts/desktop-system-one.sh          # enable System One
#   scripts/desktop-system-one.sh --off    # agent backend only
#
# Desktop's entry only needs: command = <repo>/target/release/conseqa-confluence,
# args = ["stdio"]. Restart Claude Desktop afterwards.
set -euo pipefail

dir="$HOME/.conseqa"
key_file="$dir/typesafe-key"
args_file="$dir/stdio.args"
model="${JEV_MODEL:-jev-1.13.0}"

mkdir -p "$dir"

if [[ "${1:-}" == "--off" ]]; then
  rm -f "$args_file"
  echo "System One off: removed $args_file (the key file is kept)."
  exit 0
fi

if [[ ! -s "$key_file" || "${1:-}" == "--new-key" ]]; then
  read -rsp "TypeSafe API key: " key
  echo
  [[ -n "$key" ]] || { echo "no key given" >&2; exit 1; }
  (umask 077 && printf '%s\n' "$key" > "$key_file")
  unset key
fi

cat > "$args_file" <<EOF
# Read by \`conseqa-confluence stdio\`; written by scripts/desktop-system-one.sh
--decider
system-one
--decider-url
https://api.typesafe.ai
--decider-model
$model
--decider-key-file
$key_file
--decider-log
$dir/decisions.jsonl
--system-one-kinds
operation_synthesis,topology_synthesis,requirement_discovery,requirement_repair
EOF

echo "System One on ($model). Key: $key_file. Flags: $args_file"
echo "Restart Claude Desktop to pick this up."
