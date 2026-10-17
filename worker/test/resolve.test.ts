import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { ClaudeNotFound, compareVersions, resolveClaude } from "../src/drivers/claude/resolve.ts";

const root = fs.mkdtempSync(path.join(os.tmpdir(), "helm-res-"));
after(() => fs.rmSync(root, { recursive: true, force: true }));

function touch(...parts: string[]): string {
  const p = path.join(root, ...parts);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, "dummy");
  return p;
}

// Layout: a home with ~/.local/bin, two VS Code extension versions, and a PATH dir.
const home = path.join(root, "home");
const pathDir = path.join(root, "pathdir");
const shimDir = path.join(root, "shimdir");
fs.mkdirSync(pathDir, { recursive: true });
const onPath = touch("pathdir", "claude.exe");
touch("shimdir", "claude.cmd");
const localBin = touch("home", ".local", "bin", "claude.exe");
touch("home", ".vscode", "extensions", "anthropic.claude-code-2.1.295-win32-x64", "resources", "native-binary", "claude.exe");
const newest = touch("home", ".vscode", "extensions", "anthropic.claude-code-2.1.301-win32-x64", "resources", "native-binary", "claude.exe");
touch("home", ".vscode", "extensions", "anthropic.claude-code-2.1.30-win32-x64", "resources", "native-binary", "claude.exe");
// A newer extension folder without the binary must be skipped.
fs.mkdirSync(path.join(home, ".vscode", "extensions", "anthropic.claude-code-9.9.9-win32-x64"), { recursive: true });
const explicit = touch("explicit", "my-claude.exe");

const win = { platform: "win32" as const, homedir: home };

test("HELM_CLAUDE_PATH wins over everything", () => {
  const r = resolveClaude({ ...win, env: { HELM_CLAUDE_PATH: explicit, PATH: pathDir } });
  assert.deepEqual(r, { path: explicit, source: "HELM_CLAUDE_PATH" });
});

test("HELM_CLAUDE_PATH naming a .cmd or .bat shim is refused, not skipped", () => {
  for (const name of ["claude.cmd", "claude.bat", "CLAUDE.CMD"]) {
    const shim = touch("shims2", name);
    assert.throws(() => resolveClaude({ ...win, env: { HELM_CLAUDE_PATH: shim, PATH: pathDir } }), ClaudeNotFound);
  }
});

test("HELM_CLAUDE_PATH naming a missing file is an error", () => {
  assert.throws(() => resolveClaude({ ...win, env: { HELM_CLAUDE_PATH: path.join(root, "nope.exe") } }), ClaudeNotFound);
});

test("then claude.exe on PATH; a claude.cmd on PATH is never picked", () => {
  const r = resolveClaude({ ...win, env: { PATH: `${shimDir};${pathDir}` } });
  assert.deepEqual(r, { path: onPath, source: "PATH" });
});

test("then ~/.local/bin/claude.exe", () => {
  const r = resolveClaude({ ...win, env: { PATH: shimDir } });
  assert.deepEqual(r, { path: localBin, source: "local-bin" });
});

test("then the newest VS Code extension's native binary (numeric version order)", () => {
  fs.renameSync(localBin, localBin + ".off");
  try {
    const r = resolveClaude({ ...win, env: { PATH: shimDir } });
    assert.deepEqual(r, { path: newest, source: "vscode-extension" });
  } finally {
    fs.renameSync(localBin + ".off", localBin);
  }
});

test("nothing found is a clear error", () => {
  assert.throws(() => resolveClaude({ platform: "win32", homedir: path.join(root, "empty-home"), env: { PATH: shimDir } }), /not found/);
});

test("compareVersions is numeric", () => {
  assert.ok(compareVersions("2.1.301", "2.1.295") > 0);
  assert.ok(compareVersions("2.1.30", "2.1.295") < 0);
  assert.equal(compareVersions("1.0.0", "1.0.0"), 0);
});
