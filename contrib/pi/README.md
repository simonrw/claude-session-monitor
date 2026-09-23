# csm-pi-session-monitor

Report pi session activity to [Claude Session Monitor](https://github.com/simonrw/claude-session-monitor). This package ships the TypeScript extension directly. Installation needs no build step, bundler, Rust toolchain, or JavaScript workspace.

Version 0.1.0 is prepared for local verification and has not been published by this work. The proposed npm name still needs publishability confirmation before release.

## Prerequisites

- pi, with package installation and loading tested against `@earendil-works/pi-coding-agent@0.87.1` on Node 22.19.0 or newer.
- A running `csm-watcher` on each monitored host and a reachable `csm-server`. Install these separately using the [project instructions](https://github.com/simonrw/claude-session-monitor#setup). This package does not install either service.

## Install and migrate

Once published, the intended npm installation command is:

```sh
pi install npm:csm-pi-session-monitor
```

This registers the package globally for your user in `<pi-agent-dir>/settings.json`. It does not require `npm install -g` for the extension. The default agent directory is `~/.pi/agent`; pi and the extension respect `PI_CODING_AGENT_DIR`.

Before switching from a manual installation, exit running pi sessions and remove the old `csm-session-monitor.ts` copy from `<pi-agent-dir>/extensions/`. Also remove any project copy in `.pi/extensions/` and any explicit `-e` or `extensions` setting pointing to the old file. Load only the packaged extension to avoid duplicate registry writers.

| Task after publication | Command |
| --- | --- |
| Install globally for your user | `pi install npm:csm-pi-session-monitor` |
| Install for the current project | `pi install npm:csm-pi-session-monitor --local --approve` |
| Pin the initial version globally | `pi install npm:csm-pi-session-monitor@0.1.0` |
| Pin for the current project | `pi install npm:csm-pi-session-monitor@0.1.0 --local --approve` |
| Update only this package | `pi update npm:csm-pi-session-monitor --approve` |
| Roll back globally | `pi install npm:csm-pi-session-monitor@<previous-version>` |
| Roll back in the project | `pi install npm:csm-pi-session-monitor@<previous-version> --local --approve` |
| Remove globally | `pi remove npm:csm-pi-session-monitor` |
| Remove from the project | `pi remove npm:csm-pi-session-monitor --local --approve` |
| List packages, including this project's | `pi list --approve` |

Replace `<previous-version>` with a published version. Exact versions stay pinned during updates. To move a pin, install the desired version; to follow new releases again, reinstall the unversioned source. Targeted updates apply to matching configured packages in both scopes and leave exact pins unchanged. Bare `pi update` updates pi itself in the tested baseline.

`--local` writes `.pi/settings.json`. `--approve` trusts the current project for that command; use it only in a project you trust. On subsequent pi starts, grant project trust when prompted so project packages load. Restart pi or run `/reload` after installation, updates, rollback, or removal.

These commands follow the [pi 0.87.1 package guide](https://github.com/earendil-works/pi/blob/v0.87.1/packages/coding-agent/docs/packages.md) and [CLI reference](https://github.com/earendil-works/pi/blob/v0.87.1/packages/coding-agent/docs/cli.md). The imported pi host package is a non-bundled peer dependency, as upstream requires.

## Install the local artifact now

From the repository root, pack and extract the package outside the checkout:

```sh
csm_package_dir="$HOME/.local/share/csm-pi-package/0.1.0"
mkdir -p "$csm_package_dir"
npm pack ./contrib/pi --pack-destination "$csm_package_dir"
tar -xzf "$csm_package_dir/csm-pi-session-monitor-0.1.0.tgz" -C "$csm_package_dir"
pi install "$csm_package_dir/package"
```

For a project installation, run the last command from that project with `--local --approve`. Keep the extracted directory in place because pi loads local packages by path. No `npm install` or TypeScript compilation is needed in the extracted package. Restart pi or run `/reload`.

To remove it, run `pi remove "$csm_package_dir/package"`, adding `--local --approve` for a project installation. Restart or reload before deleting the extracted directory. When moving to the published npm package, remove this local registration before installing the npm source, as they have different package identities.

## Registry behavior

Each interactive pi process writes one atomic claim at `<pi-agent-dir>/csm/sessions/<pid>.json`. It records the session ID, working directory, optional `/name`, process-start identity, and Busy or Idle status. Graceful shutdown removes the claim. After a crash, the watcher rejects stale claims using process liveness and start time. An unwritable registry triggers one warning and disables monitoring without interrupting pi.

The watcher discovers `PI_CODING_AGENT_DIR` from running pi processes and checks `~/.pi/agent`. `CSM_WATCHER_PI_REGISTRY_DIRS` is a diagnostic override containing colon-separated registry roots, normally `<pi-agent-dir>/csm`. Without a loaded extension, the watcher reports an empty pi snapshot.

## Contributor verification

```sh
cd contrib/pi
npm ci
npm run check
npm test
```

The lockfile pins the test host to pi 0.87.1. CI uses Node 22. The package test packs and extracts outside the checkout, invokes the real pi CLI for global and project installation and removal, and checks the real resource loader for exactly the intended extension. It then runs the existing lifecycle harness against the extracted file, preserving the registry schema and failure checks. The CLI and loader run offline with a temporary home, agent directory, and project. No contributor pi settings, model credentials, or paid model calls are used.

Only the source extension, this README, the license notice, and the npm manifest ship. Development dependencies and tests stay out of the tarball. Publication is a separate release task.

## License

The repository currently specifies no license grant. This package is marked `UNLICENSED`; see [LICENSE](LICENSE). Packaging does not introduce a license grant.
