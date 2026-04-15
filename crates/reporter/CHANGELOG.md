# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/simonrw/claude-session-monitor/releases/tag/csm-reporter-v0.1.0) - 2026-04-15

### Added

- shared config foundation + reporter integration (PRO-120)
- capture PostToolUse hook to clear stale tool labels (PRO-118)
- reporter Sentry integration + POST failure capture (PRO-106) ([#20](https://github.com/simonrw/claude-session-monitor/pull/20))
- Sentry foundation + server panic reporting (PRO-104) ([#18](https://github.com/simonrw/claude-session-monitor/pull/18))
- expand status derivation to handle all 7 hook events ([#3](https://github.com/simonrw/claude-session-monitor/pull/3))
- add CI and release automation workflows ([#6](https://github.com/simonrw/claude-session-monitor/pull/6))

### Other

- rename binary crates with csm- prefix
- Add clap CLI argument parsing to all binaries (PRO-93) ([#10](https://github.com/simonrw/claude-session-monitor/pull/10))
- Add hostname and VCS enrichment to reporter payload and GUI ([#5](https://github.com/simonrw/claude-session-monitor/pull/5))
- add structured logging to reporter with daily file rotation ([#4](https://github.com/simonrw/claude-session-monitor/pull/4))
- Implement tracer-bullet pipeline: reporter → server → SSE → GUI
- Set up Cargo workspace with core session types and URL resolution
