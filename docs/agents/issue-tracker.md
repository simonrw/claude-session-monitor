# Issue tracker: GitHub

Issues, PRDs, and tasks for this repo live as **GitHub issues** in
[`simonrw/claude-session-monitor`](https://github.com/simonrw/claude-session-monitor/issues).
Use the `gh` CLI for all operations. `gh` infers the repo from `git remote -v` when run inside a clone.

> Historical note: issues were migrated from Linear (team `Projects`, prefix `PRO-`) in August 2026.
> Each migrated issue keeps a `_Migrated from Linear PRO-<n>._` footer for traceability. Linear is no
> longer the source of truth.

## Conventions

- **Create an issue / PRD**: `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line
  markdown bodies. Add `--label` and, for sub-issues, wire the parent afterwards (see below).
- **Read an issue**: `gh issue view <number> --comments`.
- **List issues**: `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'`
  with `--label` / `--state` filters as needed.
- **Comment on an issue**: `gh issue comment <number> --body "..."`
- **Apply / remove labels**: `gh issue edit <number> --add-label "..."` / `--remove-label "..."`.
  Discover labels with `gh label list`.
- **Close**: `gh issue close <number> --reason completed` (or `--reason "not planned"`), optionally with
  `--comment "..."`.
- **Sub-issues (parent/child)**: GitHub native sub-issues. Wire a child to its parent with the GraphQL
  `addSubIssue` mutation, using each issue's `node_id`
  (`gh api repos/OWNER/REPO/issues/<n> --jq .node_id`):

  ```sh
  gh api graphql -f query='mutation($p:ID!,$c:ID!){addSubIssue(input:{issueId:$p,subIssueId:$c}){issue{number}}}' \
    -F p=<parent-node-id> -F c=<child-node-id>
  ```

## Pull requests as a triage surface

**PRs as a request surface: no.** _(GitHub PRs against `simonrw/claude-session-monitor` are not part of
the triage queue. Set this to `yes` and describe the workflow here if that changes; `/triage` reads this
flag.)_

## When a skill says "publish to the issue tracker"

Create a GitHub issue (`gh issue create`).

## When a skill says "fetch the relevant ticket"

Run `gh issue view <number> --comments`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: an issue labelled `wayfinder:map`, holding the Notes / Decisions-so-far / Fog body.
  `gh issue create --label wayfinder:map`.
- **Child ticket**: an issue linked to the map as a GitHub sub-issue (`addSubIssue`, above). Label
  `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`). Assign to the driving dev once claimed.
- **Blocking**: add a `**Blocked by:** #<n>, #<n>` line at the top of the blocked issue's body and apply
  the `blocked` label. A ticket is unblocked when every blocker issue is closed. (GitHub's native issue
  dependencies are a richer, UI-visible alternative if you enable them later.)
- **Frontier query**: list the map's open sub-issues, drop any whose `Blocked by` line still references an
  open issue or that already have an assignee; first in map order wins.
- **Claim**: `gh issue edit <n> --add-assignee @me` - the session's first write.
- **Resolve**: `gh issue comment <n> --body "<answer>"`, `gh issue close <n> --reason completed`, then
  append a context pointer to the map's Decisions-so-far.
