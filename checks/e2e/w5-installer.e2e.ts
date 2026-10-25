// Installer check: what the NSIS bundle must carry is read from the repository, so a new helper
// that nobody bundled fails here instead of in the installed app. (The usage and title helpers once
// shipped unbundled: "Helm could not find its usage helper".) No app is started.
//
// What it asserts:
//   - the bundle is on, NSIS only, per-user, with no updater configured;
//   - every folder under `helpers/` that has `src/main.ts` is a bundled resource at the path the
//     host looks in (`<resources>/helpers/<name>/src/`), and so is the worker source;
//   - every relative import a bundled file makes lands inside something that is bundled too;
//   - downgrades are allowed, so an older build installs over a newer one in place.
//
// Run: `npm run e2e -w checks` (cheap; reads files only).

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { describe, test } from "node:test";
import { REPO } from "./harness.ts";

const confPath = path.join(REPO, "host", "app", "tauri.conf.json");
const conf = JSON.parse(fs.readFileSync(confPath, "utf8")) as {
  version: string;
  bundle: { active: boolean; targets: string[]; resources: Record<string, string>; windows?: { allowDowngrades?: boolean; nsis?: { installMode?: string; installerHooks?: string } } };
  plugins?: Record<string, unknown>;
};

/** Resource source folders and files, as absolute paths, with the folder they land in. */
function bundled(): { src: string; dest: string; isDir: boolean }[] {
  return Object.entries(conf.bundle.resources).map(([src, dest]) => ({ src: path.resolve(path.dirname(confPath), src), dest, isDir: src.endsWith("/") }));
}

function insideBundled(file: string): boolean {
  return bundled().some((r) => (r.isDir ? file === r.src.replace(/[\\/]$/, "") || file.startsWith(r.src.replace(/[\\/]$/, "") + path.sep) : file === r.src));
}

function walk(dir: string): string[] {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(path.join(dir, e.name)) : [path.join(dir, e.name)]));
}

describe("installer: what the NSIS bundle carries", () => {
  test("a per-user NSIS bundle with no updater", () => {
    assert.equal(conf.bundle.active, true, "bundling is on");
    assert.deepEqual(conf.bundle.targets, ["nsis"], "NSIS only");
    assert.equal(conf.bundle.windows?.nsis?.installMode, "currentUser", "per-user install, no administrator prompt");
    assert.match(conf.version, /^\d+\.\d+\.\d+$/, "a plain version");
    assert.equal(conf.bundle.windows?.allowDowngrades, true, "an older build installs over a newer one (NSIS refuses a downgrade otherwise)");
    assert.equal(conf.plugins?.updater, undefined, "no updater");
    const hooks = conf.bundle.windows?.nsis?.installerHooks;
    assert.ok(hooks && fs.existsSync(path.resolve(path.dirname(confPath), hooks)), "the installer hooks file exists");
    assert.match(fs.readFileSync(path.resolve(path.dirname(confPath), hooks!), "utf8"), /Programs\\\$\{PRODUCTNAME\}/, "program files stay out of the profile folder");
  });

  test("every helper the host launches, and the worker, are bundled where the host looks", () => {
    const res = conf.bundle.resources;
    const at = (dest: string) => Object.entries(res).find(([, d]) => d === dest)?.[0];
    assert.ok(at("worker/src/"), "worker/src/ is bundled (staged at run time)");
    const helpers = fs
      .readdirSync(path.join(REPO, "helpers"), { withFileTypes: true })
      .filter((e) => e.isDirectory() && fs.existsSync(path.join(REPO, "helpers", e.name, "src", "main.ts")))
      .map((e) => e.name);
    assert.ok(helpers.length >= 3, `the helpers were found (${helpers.join(", ")})`);
    for (const name of helpers) {
      const src = at(`helpers/${name}/src/`);
      assert.ok(src, `helpers/${name}/src/ is a bundled resource: add it to bundle.resources in host/app/tauri.conf.json`);
      assert.equal(path.resolve(path.dirname(confPath), src!), path.join(REPO, "helpers", name, "src"), `it bundles the repository's own folder`);
    }
  });

  test("the installer carries the git stand-in beside helm.exe and builds it first", () => {
    // Kept out of tauri.conf.json so that debug builds, which never build the stand-in in release,
    // do not fail on a missing resource; the installer build adds it with --config.
    const overlay = JSON.parse(fs.readFileSync(path.join(REPO, "host", "app", "tauri.installer.json"), "utf8")) as { bundle: { resources: Record<string, string> } };
    assert.equal(overlay.bundle.resources["../../target/release/helm-git.exe"], "helm-git.exe", "helm-git.exe lands next to helm.exe, where the host looks for it");
    const script = (JSON.parse(fs.readFileSync(path.join(REPO, "package.json"), "utf8")) as { scripts: Record<string, string> }).scripts["build:installer"]!;
    assert.match(script, /cargo build --release -p helm-gitshim && tauri build .*--config host\/app\/tauri\.installer\.json/, "the stand-in is built before the bundle and the overlay is applied");
  });

  test("every relative import in a bundled file lands inside the bundle", () => {
    const missing: string[] = [];
    for (const r of bundled()) {
      const files = r.isDir ? walk(r.src) : [r.src];
      for (const f of files.filter((x) => /\.ts$/.test(x))) {
        const text = fs.readFileSync(f, "utf8");
        for (const m of text.matchAll(/(?:from|import)\s*\(?\s*["'](\.\.?\/[^"']+)["']/g)) {
          const target = path.resolve(path.dirname(f), m[1]!);
          if (!insideBundled(target)) missing.push(`${path.relative(REPO, f)} imports ${m[1]}`);
        }
      }
    }
    assert.deepEqual(missing, [], "these imports would not resolve in the installed app");
  });
});
