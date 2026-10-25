import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { test } from "node:test";
import { parseStatus, ShellChangeCapture } from "../src/drivers/claude/shellchanges.ts";

const git = (cwd: string, ...args: string[]) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.com", ...args], { cwd, stdio: "pipe" });

test("a shell command's changed files are found by comparing the checkout before and after", async () => {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "helm-shell-")));
  try {
    const repo = path.join(dir, "repo");
    fs.mkdirSync(repo);
    git(repo, "init", "-q", "-b", "main");
    for (const f of ["a.txt", "b.txt", "c.txt"]) fs.writeFileSync(path.join(repo, f), `${f}\n`);
    git(repo, "add", "."), git(repo, "commit", "-q", "-m", "one");
    const at = (f: string) => `${repo.replace(/\\/g, "/")}/${f}`;
    const cap = new ShellChangeCapture();
    const run = async (id: string, command: () => void): Promise<string[]> => {
      await cap.begin(id, repo);
      command();
      return (await cap.finish(id)).sort();
    };

    // A rewrite in place and a new file are both found.
    assert.deepEqual(
      await run("sed", () => {
        fs.writeFileSync(path.join(repo, "a.txt"), "a changed by a formatter\n");
        fs.writeFileSync(path.join(repo, "new.txt"), "n\n");
      }),
      [at("a.txt"), at("new.txt")],
    );
    // Putting a file back is a change too; a command that touches nothing reports nothing.
    assert.deepEqual(await run("checkout-file", () => git(repo, "checkout", "--", "a.txt")), [at("a.txt")]);
    assert.deepEqual(await run("noop", () => undefined), []);
    // Committing files that were already changed does not credit them to the commit command.
    fs.writeFileSync(path.join(repo, "b.txt"), "b edited by the agent's edit tool\n");
    assert.deepEqual(await run("commit", () => (git(repo, "add", "b.txt"), git(repo, "commit", "-q", "-m", "two"))), []);
    // Switching branches changes the files the two commits differ in, though nothing was dirty.
    git(repo, "checkout", "-q", "-b", "other");
    fs.writeFileSync(path.join(repo, "c.txt"), "c on the other branch\n");
    git(repo, "commit", "-q", "-am", "three");
    assert.deepEqual(await run("switch", () => git(repo, "checkout", "-q", "main")), [at("c.txt")]);
    // Two commands at once in one checkout cannot be told apart: neither is credited.
    await cap.begin("p1", repo);
    await cap.begin("p2", repo);
    fs.writeFileSync(path.join(repo, "a.txt"), "someone changed this\n");
    assert.deepEqual([await cap.finish("p1"), await cap.finish("p2")], [[], []]);
    // A call that never ends is dropped once too many are waiting, and a slow git is cut off.
    for (let i = 0; i < 8; i++) await cap.begin(`stuck${i}`, repo);
    await cap.begin("ninth", repo);
    assert.deepEqual(await cap.finish("ninth"), [], "past eight waiting calls, a new one is not tracked");
    cap.clear();
    const slow = new ShellChangeCapture(() => new Promise<string | null>(() => undefined), 50);
    const started = Date.now();
    await slow.begin("slow", repo);
    assert.ok(Date.now() - started < 1_000 && (await slow.finish("slow")).length === 0, "a command is not held up past the deadline");
    // Outside a checkout, or for a call that was never tracked: nothing, and no error.
    const plain = new ShellChangeCapture();
    await plain.begin("x", dir);
    assert.deepEqual([await plain.finish("x"), await cap.finish("never")], [[], []]);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("git status output is read for paths and the commit; a checkout with too many changes is skipped", () => {
  const out = ["# branch.oid abc123", "# branch.head main", "1 .M N... 100644 100644 100644 h1 h2 dir/with space.txt", "? new.txt", "u UU N... 1 2 3 4 h1 h2 h3 conflict.txt", ""].join("\0");
  assert.deepEqual(parseStatus(out), { oid: "abc123", paths: ["dir/with space.txt", "new.txt", "conflict.txt"] });
  assert.equal(parseStatus("# branch.oid (initial)\0")?.oid, "");
  assert.equal(parseStatus(Array.from({ length: 2100 }, (_, i) => `? f${i}`).join("\0")), null);
});
