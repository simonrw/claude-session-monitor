import { appendFileSync, readFileSync } from "node:fs";
import { execFileSync, spawnSync } from "node:child_process";

const command = process.argv[2];
if (!["plan", "publish"].includes(command)) throw new Error("Usage: node release.mjs plan|publish [tarball]");

const manifest = JSON.parse(readFileSync(new URL("package.json", import.meta.url), "utf8"));
const event = JSON.parse(readFileSync(process.env.GITHUB_EVENT_PATH, "utf8"));
const release = event.release;
const dryRun = process.env.GITHUB_EVENT_NAME === "workflow_dispatch";
const publish = process.env.GITHUB_EVENT_NAME === "release" && event.action === "published"
  && release?.prerelease === false && release?.draft === false
  && release.tag_name?.startsWith("csm-pi-v");

const validate = dryRun || publish;
const tag = dryRun ? event.inputs?.tag : release?.tag_name;
if (validate) {
  if (!/^csm-pi-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(tag)) {
    throw new Error(`Expected a stable csm-pi-v<major>.<minor>.<patch> tag, got ${tag}`);
  }
  if (tag !== `csm-pi-v${manifest.version}`) {
    throw new Error(`Tag ${tag} does not match package version ${manifest.version}`);
  }
}

if (process.env.GITHUB_OUTPUT) {
  appendFileSync(process.env.GITHUB_OUTPUT, `publish=${publish}\nvalidate=${validate}\n`);
}
console.log(publish ? "Eligible for publication" : "Not eligible for publication");
if (command === "publish") {
  if (!publish) throw new Error("This event is not eligible for publication");
  const tarball = process.argv[3];
  const packed = JSON.parse(execFileSync("tar", ["-xOf", tarball, "package/package.json"], { encoding: "utf8" }));
  if (packed.name !== manifest.name || packed.version !== manifest.version) {
    throw new Error("Tarball name/version does not match the tagged package");
  }
  const version = `${packed.name}@${packed.version}`;
  const lookup = spawnSync("npm", ["view", version, "version", "--json", "--registry", "https://registry.npmjs.org"], { encoding: "utf8" });
  if (lookup.status === 0) {
    throw new Error(`Version already published: ${version}. No changes made; use an explicit new version for fixes.`);
  }
  let code;
  try { code = JSON.parse(lookup.stdout).error?.code; } catch { /* Non-JSON failures also stop publication. */ }
  if (code !== "E404") throw new Error(`npm registry lookup failed: ${lookup.stderr || lookup.stdout || lookup.error}`);
  execFileSync("npm", ["publish", tarball, "--access", "public", "--ignore-scripts", "--registry", "https://registry.npmjs.org"], { stdio: "inherit" });
}
