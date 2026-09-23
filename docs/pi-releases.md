# Releasing the pi extension

`csm-pi-session-monitor` uses explicit stable semantic versions in `contrib/pi/package.json` and its lockfile. Tags are `csm-pi-v<major>.<minor>.<patch>`, starting at `csm-pi-v0.1.0`. Rust crate versions and release-plz do not control this version. Prerelease and build-metadata tags are not supported.

The [release workflow](../.github/workflows/release-pi.yml) checks out the release tag and requires its version to equal the package version. It typechecks, packs, installs and removes the extracted package through pi 0.87.1 in isolated global and project environments, checks real loading, and runs the registry lifecycle tests against the extracted extension. It retains that exact tarball, runs `npm publish --dry-run`, and uploads it as a workflow artifact. Publication downloads that artifact without repacking and uses public access.

Only a published, stable GitHub release with an extension tag can reach publication. Pushes, pull requests, other release tags, drafts, and prereleases cannot publish it. The validation job has only `contents: read`; the separate publication job adds `id-token: write`. Both use GitHub-hosted Ubuntu runners, Node 24 and npm 11.11.0, exceeding npm's minimum Node 22.14.0 and npm 11.5.1 requirements. See [npm trusted publishing](https://docs.npmjs.com/trusted-publishers/).

## Validate before npm setup

No npm account, publisher configuration, model credentials, or paid model calls are needed. Once the workflow is on the default branch, select **Actions > Release pi package > Run workflow**, supply the source branch, tag or SHA as `ref`, and the proposed extension tag as `tag`. This always performs a dry run, including version matching and all artifact checks. It cannot publish, even when given an existing release tag.

Equivalent command from the repository root:

```sh
gh workflow run release-pi.yml -f ref=main -f tag=csm-pi-v0.1.0
```

For local validation before the workflow is merged, use Node 24 and npm 11.11.0, then run:

```sh
cd contrib/pi
csm_validation_dir=$(mktemp -d)
printf '%s\n' '{"inputs":{"tag":"csm-pi-v0.1.0"}}' > "$csm_validation_dir/event.json"
GITHUB_EVENT_NAME=workflow_dispatch GITHUB_EVENT_PATH="$csm_validation_dir/event.json" node release.mjs plan
npm ci
npm run check
CSM_PI_TARBALL="$csm_validation_dir/package.tgz" npm test
npm publish "$csm_validation_dir/package.tgz" --dry-run --access public --ignore-scripts --registry https://registry.npmjs.org
```

Run the commands in order and stop on any failure. Change the proposed tag when validating a different version. The package test refuses to overwrite an existing destination, so use a new temporary directory for each attempt. Keep the successful logs and tarball for inspection. This proves local artifact validation, not GitHub OIDC authentication or public-registry installation.

## First-package bootstrap and access

Actual publication requires a maintainer with GitHub repository write access to push the tag and publish releases, plus npm publishing rights to the final package name. Configuring trusted publishers requires access to the package's npm settings. Confirm the name and ownership before release; a registry 404 does not reserve a name or prove publishing rights. Resolve naming or licensing decisions explicitly before publishing. The current package is `UNLICENSED` and supplies no license grant.

npm's trusted-publisher settings belong to an existing package. If this is a new package, a maintainer must first publish the validated tarball interactively from their own machine with their npm account and required 2FA:

```sh
npm login
npm publish /absolute/path/to/validated/package.tgz --access public --ignore-scripts --registry https://registry.npmjs.org
```

Use the tarball from the successful dry run for that exact source and version. Do not put a persistent npm write token in GitHub secrets or fetch credentials through 1Password. Bootstrap consumes that version permanently. Configure trusted publishing next, then explicitly bump to a distinct version for the first automated publication, for example `0.1.1` after bootstrapping `0.1.0`.

In the npm package's **Settings > Trusted publishing**, add a GitHub Actions publisher with these exact values:

| Setting | Value |
| --- | --- |
| Organization or user | `simonrw` |
| Repository | `claude-session-monitor` |
| Workflow filename | `release-pi.yml` |
| Environment name | Leave empty, this workflow has no GitHub environment |
| Allowed actions | Enable direct `npm publish` |

Enter only the workflow filename, without `.github/workflows/`. The package repository URL must identify this GitHub repository. Routine releases use OIDC and need no `NPM_TOKEN` or `NODE_AUTH_TOKEN`. After verifying automated publication, npm recommends **Require two-factor authentication and disallow tokens** under publishing access. npm automatically attaches provenance for trusted publication from a public repository. See the [official configuration and troubleshooting guide](https://docs.npmjs.com/trusted-publishers/).

## Release sequence

1. Make the extension change. For later releases, run `npm version <version> --no-git-tag-version` in `contrib/pi` to update the manifest and lockfile explicitly. Review and commit both files. Keep installation examples accurate when their documented baseline changes.
2. Run the dry-run workflow against the intended commit with the matching proposed tag. Inspect the validation result and download the tarball if needed.
3. Create and push `csm-pi-v<version>` at that same reviewed commit. Publish a GitHub release for that existing tag, with prerelease disabled. Avoid marking an extension release as the repository's latest Rust release, for example use `gh release create csm-pi-v<version> --verify-tag --latest=false --title 'pi extension <version>' --notes-file <release-notes-file>`.
4. Watch **Release pi package**. Validation runs again on the tagged source. Only its successful artifact can reach the publication job. The Rust binary job excludes `csm-pi-` tags; existing Rust and macOS routing is preserved.
5. Record the release URL, successful run and npm version. Follow [#206](https://github.com/simonrw/claude-session-monitor/issues/206) for the first public install, loading and removal acceptance test. A successful dry run does not complete that ticket.

## Retry and recovery

- A version mismatch or failed package check stops before publication. Fix the source and validate again. Do not move a published release tag; choose and commit a new version when released source needs correction.
- An already-published version fails with an explicit message and makes no changes. Check whether a previous attempt or bootstrap succeeded. Confirm the registry artifact before treating the release as complete. Never bump automatically, overwrite an artifact, unpublish to reuse a version, or move tags to retry.
- Registry failures other than an explicit npm `E404` stop publication. Fix the network or registry problem and rerun. npm also rejects a duplicate if another publisher wins the race after the lookup.
- For OIDC errors, check the exact settings above, package ownership, Node/npm versions, and the job's OIDC permission. Correct external configuration and rerun the failed publication job. It downloads the original validation artifact by ID and checks out the original validated commit. If the artifact has expired, rerun all jobs to validate a fresh artifact from the same tagged source.
- An artifact upload uses a distinct name for each full workflow attempt. No retries overwrite previous artifacts. Dry runs never check npm ownership or claim that trusted publishing has been configured correctly.

## Recorded non-publishing validation

On 2026-09-23, the local dry-run sequence above passed on macOS arm64 using Node 24.21.0, npm 11.11.0 and pi 0.87.1. The source was the implementation of [#205](https://github.com/simonrw/claude-session-monitor/issues/205) based on `bc6b79f53c47b7c0deff5f1a595bdcf77716a94e`, with the proposed tag `csm-pi-v0.1.0`.

- The event plan returned `publish=false` and `validate=true`.
- `npm ci`, typechecking, and all 18 extension/package/release tests passed, including the real installer, loader, global and project removal, and extracted-extension lifecycle checks.
- npm's public-access publish dry run passed on the retained four-file tarball, without publishing.
- Tarball SHA-256: `c4197d1d00610c3d5ddc7bf0eb43804f45f6b1b883cb7fb1fc5f3068a6619323`.
- Workflow validation with actionlint 1.7.12 passed. Rust workspace tests and the web tests/build also passed; the web build reported existing CSS warnings.

This is local validation evidence. A GitHub-hosted run, npm bootstrap, OIDC publication and public-registry installation acceptance remain separate work under [#206](https://github.com/simonrw/claude-session-monitor/issues/206).
