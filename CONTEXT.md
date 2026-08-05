# Glossary

Terms with a precise meaning in this project. Sources: doc comments in `crates/common/src/presentation.rs`, `crates/common/src/api.rs`, and design sessions.

- **Session**: one agent (Claude Code / Codex) conversation observed by a watcher on a host. Carried to frontends as a `SessionView`.
- **Status**: the session's current state: Busy, Shell, Idle, Waiting, or Ended. Waiting is unconditionally the state that most wants the user's attention.
- **Waiting section**: the top portion of the session list holding sessions whose status is Waiting - "needs me right now".
- **Rest section**: everything else, sorted most-recently-updated first. Deliberately not called "working": it also holds Idle and Ended sessions.
- **Stale**: a session untouched for 30 minutes; distinct from a watcher going silent at the host level.
- **Faded / dimmed**: rendered de-saturated because the client is disconnected or the session is stale.
- **De-emphasised**: rendered subdued because the session has no tmux target (cannot be activated); deliberately distinct from faded.
- **Watcher-silent**: the empty-list explanation used when no watcher is reporting (as opposed to "genuinely no sessions").
- **Activation**: jumping to a session's tmux pane (possibly over ssh).
- **Switcher mode**: running the TUI with `--exit-on-select` as a one-shot session picker.
- **Session identity**: the fields that say *which* session this is: the `/rename` name if set, otherwise host plus working directory. Identity is never elided at any width.
- **Floor width**: the narrowest terminal the TUI commits to being fully informative in: 40 columns. Below the floor, rendering may degrade arbitrarily.
- **Breakpoint**: a terminal width at which the TUI switches between discrete named layouts. Layouts do not scale continuously; they switch. There is one breakpoint: 80 columns.
- **Card**: the below-breakpoint rendering of one session: a small multi-line block instead of a single row line.
- **Detail view**: a read-only, full-screen view of everything known about one session, opened from the list with a shortcut, available at every width. It replaces the list while open, shows exactly one session, and offers no interaction beyond closing.
