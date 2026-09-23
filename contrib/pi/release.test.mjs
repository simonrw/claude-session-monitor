import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { parse } from "yaml";

const releaseScript = fileURLToPath(new URL("release.mjs", import.meta.url));

function runRelease(t, eventName, event, command = "plan", registry = "missing") {
  const root = mkdtempSync(join(tmpdir(), "csm-pi-release-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const eventPath = join(root, "event.json");
  const output = join(root, "output");
  writeFileSync(eventPath, JSON.stringify(event));
  writeFileSync(output, "");
  // A fixture version keeps these release scenarios valid across real version bumps.
  copyFileSync(releaseScript, join(root, "release.mjs"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "csm-pi-session-monitor", version: "0.1.0" }));
  mkdirSync(join(root, "package"));
  writeFileSync(join(root, "package", "package.json"), JSON.stringify({ name: "csm-pi-session-monitor", version: "0.1.0" }));
  const tarball = join(root, "package.tgz");
  execFileSync("tar", ["-czf", tarball, "-C", root, "package"]);
  // Substitute only the npm process boundary, never contact the registry in tests.
  writeFileSync(join(root, "npm"), `#!${process.execPath}
    const fs = require("node:fs");
    const args = process.argv.slice(2);
    if (args[0] === "view") {
      if (process.env.TEST_REGISTRY === "published") console.log('"0.1.0"');
      else {
        console.log(JSON.stringify({ error: { code: process.env.TEST_REGISTRY === "missing" ? "E404" : "E503" } }));
        process.exit(1);
      }
    } else if (args[0] === "publish") {
      fs.writeFileSync(process.env.TEST_PUBLISHED, JSON.stringify({ args, bytes: fs.readFileSync(args[1]).toString("base64") }));
    } else {
      throw new Error("Unexpected npm command: " + args.join(" "));
    }
  `, { mode: 0o755 });
  const publishedPath = join(root, "published.json");
  const result = spawnSync(process.execPath, [join(root, "release.mjs"), command, tarball], {
    env: {
      ...process.env, PATH: `${root}:${process.env.PATH}`, TEST_REGISTRY: registry, TEST_PUBLISHED: publishedPath,
      GITHUB_EVENT_NAME: eventName, GITHUB_EVENT_PATH: eventPath, GITHUB_OUTPUT: output,
    },
    encoding: "utf8",
  });
  let published;
  try { published = JSON.parse(readFileSync(publishedPath, "utf8")); } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  return { ...result, output: readFileSync(output, "utf8"), published, tarball, bytes: readFileSync(tarball).toString("base64") };
}

test("a published stable extension tag matching the package is eligible", (t) => {
  const result = runRelease(t, "release", {
    action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: false },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.output, /publish=true/);
});

test("publication sends the exact validated tarball with public access", (t) => {
  const result = runRelease(t, "release", {
    action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: false },
  }, "publish");
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(result.published, {
    args: ["publish", result.tarball, "--access", "public", "--ignore-scripts", "--registry", "https://registry.npmjs.org"],
    bytes: result.bytes,
  });
});

for (const [registry, message] of [
  ["published", /already published.*0.1.0/],
  ["unavailable", /registry lookup failed/],
]) {
  test(`${registry} registry result stops publication clearly`, (t) => {
    const result = runRelease(t, "release", {
      action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: false },
    }, "publish", registry);
    assert.equal(result.status, 1);
    assert.match(result.stderr, message);
    assert.equal(result.published, undefined);
  });
}

test("an extension tag with a mismatched version fails validation", (t) => {
  const result = runRelease(t, "release", {
    action: "published", release: { tag_name: "csm-pi-v0.2.0", prerelease: false, draft: false },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /does not match package version/);
});

for (const [name, eventName, event] of [
  ["unrelated tag", "release", { action: "published", release: { tag_name: "csm-server-v0.1.0", prerelease: false, draft: false } }],
  ["prerelease", "release", { action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: true, draft: false } }],
  ["draft", "release", { action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: true } }],
  ["unpublished release", "release", { action: "created", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: false } }],
  ["pull request", "pull_request", {}],
  ["ordinary push", "push", {}],
]) {
  test(`${name} cannot publish`, (t) => {
    const result = runRelease(t, eventName, event, "publish");
    assert.equal(result.status, 1);
    assert.match(result.stderr, /not eligible for publication/);
  });
}

test("a manual dry run checks its proposed tag but cannot publish", (t) => {
  const event = { inputs: { tag: "csm-pi-v0.1.0" } };
  const plan = runRelease(t, "workflow_dispatch", event);
  assert.equal(plan.status, 0, plan.stderr);
  assert.match(plan.output, /publish=false/);
  assert.match(plan.output, /validate=true/);
  const publish = runRelease(t, "workflow_dispatch", event, "publish");
  assert.equal(publish.status, 1);
  assert.match(publish.stderr, /not eligible for publication/);
  const mismatch = runRelease(t, "workflow_dispatch", { inputs: { tag: "csm-pi-v0.2.0" } });
  assert.equal(mismatch.status, 1);
});

// Evaluate the workflows' actual routing expressions with representative events.
// These expressions use the shared JavaScript/Actions boolean subset and startsWith.
function routes(workflowName, jobName, eventName, event, needs = {}) {
  const workflow = parse(readFileSync(new URL(`../../.github/workflows/${workflowName}`, import.meta.url), "utf8"));
  if (!(eventName in workflow.on)) return false;
  if (workflow.on[eventName]?.types && !workflow.on[eventName].types.includes(event.action)) return false;
  const job = workflow.jobs[jobName];
  for (const dependency of [job.needs ?? []].flat()) {
    if (needs[dependency]?.result !== "success") return false;
  }
  return Boolean(runInNewContext(job.if, {
    github: { event_name: eventName, event }, needs,
    startsWith: (value, prefix) => value.startsWith(prefix),
  }));
}

test("release workflows route extension, Rust, and macOS releases independently", () => {
  for (const [tag, extension, rust, mac] of [
    ["csm-pi-v0.1.0", true, false, false],
    ["csm-server-v0.2.0", false, true, false],
    ["csm-mac-v0.3.0", false, false, true],
    ["common-v0.4.0", false, false, false],
  ]) {
    const event = { action: "published", release: { tag_name: tag, prerelease: false, draft: false } };
    assert.equal(routes("release-pi.yml", "validate", "release", event), extension, tag);
    assert.equal(routes("release-binaries.yml", "build", "release", event), rust, tag);
    assert.equal(routes("release-mac-app.yml", "build-dmg", "release", event), mac, tag);
  }
});

test("workflow publication requires successful validation of a release event", () => {
  for (const [eventName, event, validate, publish] of [
    ["release", { action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: false, draft: false } }, true, true],
    ["release", { action: "published", release: { tag_name: "csm-pi-v0.1.0", prerelease: true, draft: false } }, false, false],
    ["workflow_dispatch", { inputs: { tag: "csm-pi-v0.1.0" } }, true, false],
    ["pull_request", {}, false, false],
    ["push", {}, false, false],
  ]) {
    assert.equal(routes("release-pi.yml", "validate", eventName, event), validate);
    const needs = { validate: { result: "success", outputs: { publish: String(publish) } } };
    assert.equal(routes("release-pi.yml", "publish", eventName, event, needs), publish);
    needs.validate.result = "failure";
    assert.equal(routes("release-pi.yml", "publish", eventName, event, needs), false);
  }
});

test("prerelease and malformed proposed version tags fail validation", (t) => {
  for (const tag of ["csm-pi-v0.1.0-rc.1", "csm-pi-v01.0.0", "csm-pi-v0.1.0+build", "csm-pi-v0.1"]) {
    const result = runRelease(t, "workflow_dispatch", { inputs: { tag } });
    assert.equal(result.status, 1, tag);
    assert.match(result.stderr, /Expected a stable/);
    assert.equal(result.published, undefined);
  }
});
