# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/simonrw/claude-session-monitor/compare/common-v0.1.0...common-v0.2.0) - 2026-04-15

### Added

- add macOS vibrancy/blur effect (PRO-112) ([#26](https://github.com/simonrw/claude-session-monitor/pull/26))
- shared config foundation + reporter integration (PRO-120)
- GUI Sentry integration + SSE failure capture (PRO-107) ([#19](https://github.com/simonrw/claude-session-monitor/pull/19))
- Sentry foundation + server panic reporting (PRO-104) ([#18](https://github.com/simonrw/claude-session-monitor/pull/18))
- add connection status indicator to GUI (PRO-98) ([#13](https://github.com/simonrw/claude-session-monitor/pull/13))
- comprehensive tracing for server, GUI, and common crates (PRO-97) ([#12](https://github.com/simonrw/claude-session-monitor/pull/12))
- add CI and release automation workflows ([#6](https://github.com/simonrw/claude-session-monitor/pull/6))

### Other

- set default server URL to Tailscale endpoint
- Add hostname and VCS enrichment to reporter payload and GUI ([#5](https://github.com/simonrw/claude-session-monitor/pull/5))
- Implement tracer-bullet pipeline: reporter → server → SSE → GUI
- Set up Cargo workspace with core session types and URL resolution
