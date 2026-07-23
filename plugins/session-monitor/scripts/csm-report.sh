#!/bin/sh
# Claude Session Monitor hook wrapper.
#
# Resolves the csm-reporter binary and forwards the hook event (stdin) plus any
# arguments to it. This script always exits 0 so a missing or failing reporter
# never blocks the agent.
#
# Resolution order:
#   1. $CSM_REPORTER_BIN (if set and executable)
#   2. csm-reporter on PATH
#   3. $CARGO_HOME/bin/csm-reporter (or ~/.cargo/bin/csm-reporter)

log_dir="${HOME}/.local/share/claude-session-monitor"

resolve_bin() {
  if [ -n "${CSM_REPORTER_BIN:-}" ] && [ -x "${CSM_REPORTER_BIN}" ]; then
    printf '%s\n' "${CSM_REPORTER_BIN}"
    return 0
  fi

  if bin=$(command -v csm-reporter 2>/dev/null); then
    printf '%s\n' "${bin}"
    return 0
  fi

  cargo_bin="${CARGO_HOME:-${HOME}/.cargo}/bin/csm-reporter"
  if [ -x "${cargo_bin}" ]; then
    printf '%s\n' "${cargo_bin}"
    return 0
  fi

  return 1
}

if ! bin=$(resolve_bin); then
  # Reporter not installed. Note it once and soft-fail so the agent is never
  # blocked. Install with: cargo install --path crates/reporter --locked
  mkdir -p "${log_dir}" 2>/dev/null
  printf '%s csm-report.sh: csm-reporter not found on PATH/$CSM_REPORTER_BIN/$CARGO_HOME; skipping report\n' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null)" >>"${log_dir}/reporter.log" 2>/dev/null
  exit 0
fi

# Forward stdin (the hook event JSON) and any args to the reporter.
"${bin}" "$@" || true

exit 0
