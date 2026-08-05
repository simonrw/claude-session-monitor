# Tracking

* Store PRDs and tasks as GitHub issues in the `simonrw/claude-session-monitor` repo, via the `gh` CLI

## Agent skills

### Issue tracker

Issues, PRDs, and tasks live as GitHub issues in `simonrw/claude-session-monitor`, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`) as GitHub labels on the repo. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.

# Architecture

Cargo workspace: `crates/{common,server,reporter,watcher,gui,tui,core-ffi,test-support}`.

## Common crate design

- Organise modules vertically by functionality, not by code structure
- Each module exposes a small, stable interface to decouple modules from each other
- Tests at module boundaries only, no unit tests

# Version control

We use conventional commits (https://www.conventionalcommits.org/en/v1.0.0/) to track features and designs and to trigger the right releases. The commit message prefixes are:

* New features: "feat: "
* Bug fixes: "fix: "
* Breaking changes: "feat!: " or "fix!: "
* Otherwise: "chore: "
