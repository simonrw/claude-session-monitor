import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFile, mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const packageDir = fileURLToPath(new URL(".", import.meta.url));

test("the packed package installs, loads, reports sessions, and can be removed", async () => {
  const root = await realpath(await mkdtemp(join(tmpdir(), "csm-pi-package-")));
  try {
    const [packed] = JSON.parse(execFileSync("npm", [
      "pack", "--json", "--pack-destination", root,
    ], { cwd: packageDir, encoding: "utf8" }));
    assert.deepEqual(packed.files.map(({ path }) => path).sort(), [
      "LICENSE", "README.md", "csm-session-monitor.ts", "package.json",
    ]);
    execFileSync("tar", ["-xzf", join(root, packed.filename), "-C", root]);
    const manifest = JSON.parse(await readFile(join(root, "package", "package.json"), "utf8"));
    assert.deepEqual(manifest.pi.extensions, ["./csm-session-monitor.ts"]);
    for (const extension of manifest.pi.extensions) {
      await readFile(join(root, "package", extension));
    }

    const extracted = join(root, "package");
    const project = join(root, "project");
    await mkdir(project);
    const env = {
      PATH: process.env.PATH,
      HOME: root,
      PI_CODING_AGENT_DIR: join(root, "agent"),
      PI_OFFLINE: "1",
    };
    const cli = fileURLToPath(new URL(
      "node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js", import.meta.url,
    ));
    const pi = (...args) => execFileSync(process.execPath, [cli, ...args], {
      cwd: project, env, encoding: "utf8", timeout: 30_000,
    });
    const sdk = import.meta.resolve("@earendil-works/pi-coding-agent");
    const verifyLoader = (expected) => execFileSync(process.execPath, [
      "--input-type=module", "--eval", `
        import assert from "node:assert/strict";
        import { DefaultResourceLoader } from ${JSON.stringify(sdk)};
        const loader = new DefaultResourceLoader({
          cwd: process.cwd(), agentDir: process.env.PI_CODING_AGENT_DIR,
        });
        await loader.reload({ resolveProjectTrust: async () => true });
        const { extensions, errors } = loader.getExtensions();
        assert.deepEqual(errors, []);
        assert.deepEqual(extensions.map(extension => extension.resolvedPath), ${JSON.stringify(expected)});
      `,
    ], { cwd: project, env, encoding: "utf8", timeout: 30_000 });

    assert.equal(pi("--version").trim(), "0.87.1");
    for (const flags of [[], ["--local", "--approve"]]) {
      pi("install", extracted, ...flags);
      const listed = pi("list", "--approve");
      assert.ok(listed.includes(extracted), listed);
      verifyLoader([join(extracted, "csm-session-monitor.ts")]);
      pi("remove", extracted, ...flags);
      assert.ok(!pi("list", "--approve").includes(extracted));
      verifyLoader([]);
    }

    // Run the existing event-to-registry contract from outside the checkout too.
    const lifecycleTest = join(root, "csm-session-monitor.test.mjs");
    await copyFile(join(packageDir, "csm-session-monitor.test.mjs"), lifecycleTest);
    execFileSync(process.execPath, ["--test", lifecycleTest], {
      cwd: project,
      env: { ...env, CSM_PI_EXTENSION: join(extracted, "csm-session-monitor.ts") },
      encoding: "utf8", timeout: 30_000,
    });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
