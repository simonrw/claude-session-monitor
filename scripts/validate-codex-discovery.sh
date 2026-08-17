#!/usr/bin/env bash

set -euo pipefail

for command in cargo jq pgrep; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required command is unavailable: $command" >&2
    exit 1
  fi
done

if pgrep -x csm-watcher >/dev/null; then
  echo "csm-watcher is already running; stop it before running this validation" >&2
  pgrep -alf csm-watcher >&2
  exit 1
fi

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
validation_dir=$(mktemp -d "${TMPDIR:-/tmp}/csm-codex-validation.XXXXXX")
trap 'rm -rf "$validation_dir"' EXIT
mkdir "$validation_dir/empty-claude-registry"

cd "$repo_root"
cargo build --quiet --package csm-watcher
target_dir=$(cargo metadata --no-deps --format-version 1 | jq -r '.target_directory')
watcher="$target_dir/debug/csm-watcher"

snapshot() {
  CSM_WATCHER_REGISTRY_DIRS="$validation_dir/empty-claude-registry" \
    "$watcher" --once --print-sessions \
    | jq '[.[] | select(.agent_kind == "codex") | .sessions[] | {session_id, cwd, name}] | sort_by(.session_id)'
}

echo "Capturing baseline Codex sessions..."
snapshot >"$validation_dir/before.json"
jq -r '.[] | "  \(.session_id)  \(.cwd)"' "$validation_dir/before.json"

echo
echo "Start one new Codex CLI session in another terminal and leave it at the prompt."
read -r -p "Press Return when it is ready... "

if pgrep -x csm-watcher >/dev/null; then
  echo "csm-watcher started during validation; results would be ambiguous" >&2
  exit 1
fi

snapshot >"$validation_dir/after.json"
jq --slurpfile before "$validation_dir/before.json" \
  --slurpfile after "$validation_dir/after.json" \
  -n '$after[0] | map(select(.session_id as $id | ($before[0] | map(.session_id) | index($id) | not)))' \
  >"$validation_dir/new.json"

new_count=$(jq 'length' "$validation_dir/new.json")
if [[ "$new_count" -ne 1 ]]; then
  echo "FAIL: expected exactly one newly discovered Codex session, found $new_count" >&2
  echo "Before:" >&2
  jq . "$validation_dir/before.json" >&2
  echo "After:" >&2
  jq . "$validation_dir/after.json" >&2
  exit 1
fi

echo "PASS: exactly one new Codex session was discovered:"
jq . "$validation_dir/new.json"
