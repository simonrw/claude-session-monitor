# Tracking

* Store PRDs and tasks in linear under the "Claude Session Monitor" project within the "Projects" team

## Agent skills

### Issue tracker

Issues, PRDs, and tasks live in Linear (project "Claude Session Monitor", team "Projects", prefix `PRO-`) via the Linear MCP tools. See `docs/agents/issue-tracker.md`.

### Triage labels

Default canonical labels (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`) as Linear labels on the "Projects" team. See `docs/agents/triage-labels.md`.

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
