#!/usr/bin/env bash
# Points Claude Desktop's `conseqa` MCP server at this checkout's release
# build, with System One (hosted Jev) building requirement discovery and
# repair in process. Prompts for the TypeSafe key without echoing it and
# writes it only into the Desktop config's env block for this server.
#
#   scripts/desktop-system-one.sh          # enable System One
#   scripts/desktop-system-one.sh --off    # agent backend only
#
# Restart Claude Desktop afterwards.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
config="$HOME/Library/Application Support/Claude/claude_desktop_config.json"
model="${JEV_MODEL:-jev-1.13.0}"
claude_bin="$(command -v claude || echo "$HOME/.local/bin/claude")"

key=""
if [[ "${1:-}" != "--off" ]]; then
  read -rsp "TypeSafe API key: " key
  echo
  [[ -n "$key" ]] || { echo "no key given" >&2; exit 1; }
fi

cp "$config" "$config.bak"

KEY="$key" REPO="$repo" MODEL="$model" CLAUDE_BIN="$claude_bin" python3 - "$config" <<'EOF'
import json, os, sys

path = sys.argv[1]
config = json.load(open(path))
data = os.path.expanduser("~/.conseqa")

server = {
    "command": f"{os.environ['REPO']}/target/release/conseqa-confluence",
    "args": ["stdio", "--backend-program", os.environ["CLAUDE_BIN"]],
}

if os.environ["KEY"]:
    server["args"] += [
        "--decider", "system-one",
        "--decider-url", "https://api.typesafe.ai",
        "--decider-model", os.environ["MODEL"],
        "--decider-key-env", "TYPESAFE_API_KEY",
        "--decider-log", f"{data}/decisions.jsonl",
        "--system-one-kinds", "requirement_discovery,requirement_repair",
    ]
    server["env"] = {"TYPESAFE_API_KEY": os.environ["KEY"]}

config.setdefault("mcpServers", {})["conseqa"] = server

with open(path, "w") as out:
    json.dump(config, out, indent=2)

os.chmod(path, 0o600)
print("conseqa:", " ".join(server["args"]))
EOF

echo "Restart Claude Desktop to pick this up. Backup: $config.bak"
