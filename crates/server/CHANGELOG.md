# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/simonrw/claude-session-monitor/releases/tag/csm-server-v0.1.0) - 2026-04-15

### Added

- server route error handling + Sentry capture (PRO-105) ([#21](https://github.com/simonrw/claude-session-monitor/pull/21))
- Sentry foundation + server panic reporting (PRO-104) ([#18](https://github.com/simonrw/claude-session-monitor/pull/18))
- comprehensive tracing for server, GUI, and common crates (PRO-97) ([#12](https://github.com/simonrw/claude-session-monitor/pull/12))
- add CI and release automation workflows ([#6](https://github.com/simonrw/claude-session-monitor/pull/6))

### Other

- rename binary crates with csm- prefix
- Add clap CLI argument parsing to all binaries (PRO-93) ([#10](https://github.com/simonrw/claude-session-monitor/pull/10))
- install pre-commit
- add reporter→server→SSE integration tests
- two-section layout, color coding, staleness fading, and session delete (PRO-89) ([#8](https://github.com/simonrw/claude-session-monitor/pull/8))
- Add hostname and VCS enrichment to reporter payload and GUI ([#5](https://github.com/simonrw/claude-session-monitor/pull/5))
- Implement tracer-bullet pipeline: reporter → server → SSE → GUI
- Set up Cargo workspace with core session types and URL resolution
