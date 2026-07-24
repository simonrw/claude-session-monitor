# Issue tracker: Linear

Issues, PRDs, and tasks for this repo live in **Linear**, in the **Claude Session Monitor**
project within the **Projects** team (issue prefix `PRO-`). Use the Linear MCP tools
(`mcp__linear-server__*`) for all operations.

- Project: `Claude Session Monitor` (id `3b664b79-a787-4f9c-a881-5471c70bc478`)
- Team: `Projects` / key `PRO` (id `6931e88c-856c-413d-9508-f8677036f345`)
- URL: https://linear.app/srw-projects/project/claude-session-monitor-40101d168a95

## Conventions

- **Create an issue / PRD**: `save_issue` with `title`, `description` (markdown), `team: "Projects"`,
  and `project: "Claude Session Monitor"`. Add `labels` and `parentId` as needed. Send markdown
  content directly - real newlines, no escaped `\n`.
- **Read an issue**: `get_issue` with the identifier (e.g. `PRO-123`). Use `list_comments` for the
  discussion thread.
- **List issues**: `list_issues` scoped with `project: "Claude Session Monitor"` (and `team: "Projects"`),
  plus `label`, `state`, or `assignee` filters as needed.
- **Comment on an issue**: `save_comment` with the issue id and `body`.
- **Apply / remove labels**: `save_issue` with the updated `labels` array (labels replace, so include the
  full set you want). Discover label names with `list_issue_labels` (`team: "Projects"`).
- **Close**: `save_issue` setting `state` to a completed/canceled status (e.g. `Done`, `Canceled`).
  Look up available statuses with `list_issue_statuses` for the `Projects` team.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(GitHub PRs against `simonrw/claude-session-monitor` are not part of
the Linear triage queue. Set this to `yes` and describe the workflow here if that changes.)_

## When a skill says "publish to the issue tracker"

Create a Linear issue in the `Claude Session Monitor` project (`save_issue`).

## When a skill says "fetch the relevant ticket"

Call `get_issue` with the `PRO-<n>` identifier, and `list_comments` for its thread.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single Linear issue with **child** issues as tickets.

- **Map**: an issue in the `Claude Session Monitor` project labelled `wayfinder:map`, holding the
  Notes / Decisions-so-far / Fog body.
- **Child ticket**: an issue with the map set as its `parentId` (Linear sub-issue). Label
  `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Assign to the driving dev once claimed.
- **Blocking**: use Linear issue relations of type `blocks` / `blocked by`. Where a relation can't be set,
  fall back to a `Blocked by: PRO-<n>, PRO-<n>` line at the top of the child body. A ticket is unblocked
  when every blocker is in a completed/canceled state.
- **Frontier query**: list the map's open sub-issues, drop any with an open blocker or an assignee;
  first in map order wins.
- **Claim**: assign the issue to the current dev - the session's first write.
- **Resolve**: add a comment with the answer (`save_comment`), move the issue to `Done`, then append a
  context pointer to the map's Decisions-so-far.
