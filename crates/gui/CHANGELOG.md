# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/simonrw/claude-session-monitor/releases/tag/csm-gui-v0.1.0) - 2026-04-15

### Added

- add click-through mode toggle (PRO-111) ([#25](https://github.com/simonrw/claude-session-monitor/pull/25))
- add --hide-from-dock CLI flag for macOS (PRO-110) ([#24](https://github.com/simonrw/claude-session-monitor/pull/24))
- add transparent overlay window toggle (PRO-108) ([#23](https://github.com/simonrw/claude-session-monitor/pull/23))
- add macOS vibrancy/blur effect (PRO-112) ([#26](https://github.com/simonrw/claude-session-monitor/pull/26))
- add borderless window mode toggle (PRO-109) ([#22](https://github.com/simonrw/claude-session-monitor/pull/22))
- GUI integration with shared config (PRO-121)
- shared config foundation + reporter integration (PRO-120)
- GUI Sentry integration + SSE failure capture (PRO-107) ([#19](https://github.com/simonrw/claude-session-monitor/pull/19))
- Sentry foundation + server panic reporting (PRO-104) ([#18](https://github.com/simonrw/claude-session-monitor/pull/18))
- add always-on-top toggle to GUI menu bar (PRO-96) ([#14](https://github.com/simonrw/claude-session-monitor/pull/14))
- macOS .dmg packaging for GUI releases ([#15](https://github.com/simonrw/claude-session-monitor/pull/15))
- add connection status indicator to GUI (PRO-98) ([#13](https://github.com/simonrw/claude-session-monitor/pull/13))
- comprehensive tracing for server, GUI, and common crates (PRO-97) ([#12](https://github.com/simonrw/claude-session-monitor/pull/12))

### Other

- rename binary crates with csm- prefix
- Add clap CLI argument parsing to all binaries (PRO-93) ([#10](https://github.com/simonrw/claude-session-monitor/pull/10))
- two-section layout, color coding, staleness fading, and session delete (PRO-89) ([#8](https://github.com/simonrw/claude-session-monitor/pull/8))
- Add hostname and VCS enrichment to reporter payload and GUI ([#5](https://github.com/simonrw/claude-session-monitor/pull/5))
- Implement tracer-bullet pipeline: reporter → server → SSE → GUI
- Set up Cargo workspace with core session types and URL resolution
