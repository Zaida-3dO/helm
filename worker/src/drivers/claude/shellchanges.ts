// Which files a shell command changed, for the turn's change card. The edit tools say what they
// changed; a command (sed, a formatter, git checkout) does not, so the worker compares the
// checkout before and after each Bash call: `git status` lists the files that differ from the last
// commit, and a file counts as changed when its size or modification time moved, when it newly
// appears in the list, or when the command moved HEAD and the file differs between the two commits.
//
// It is a best effort and never blocks or fails a command: only the chat's own folder is looked at
// (when it is inside a git checkout), every git call has a short time limit, a checkout with too
// many changed files is skipped, and a command that overlapped another one in the same checkout is
// not credited with anything, because the changes cannot be told apart.

import { execFile } from "node:child_process";
import { promises as fs } from "node:fs";
import * as path from "node:path";

export const SHELL_CHANGES_HOOK_ID = "helm-shell-changes";
export const GIT_TIMEOUT_MS = 1_500;
/** A checkout listing more changed files than this is skipped. */
export const MAX_LISTED = 2_000;
/** Paths reported for one command. */
export const MAX_REPORTED = 200;

type Git = (args: string[], cwd: string) => Promise<string | null>;

/** Runs git without taking optional locks (so it never gets in the way of the agent's own git). */
export const runGit: Git = (args, cwd) =>
  new Promise((resolve) => {
    execFile("git", ["--no-optional-locks", ...args], { cwd, timeout: GIT_TIMEOUT_MS, maxBuffer: 4 * 1024 * 1024, windowsHide: true, encoding: "utf8" }, (err, stdout) => resolve(err ? null : stdout));
  });

interface Snapshot {
  /** The commit HEAD pointed at, or "" for a repository without one. */
  oid: string;
  /** Changed-or-untracked files (root-relative, forward slashes) and a signature of each one's file on disk. */
  dirty: Map<string, string>;
}

/** The checkout containing `dir` (the nearest folder with a `.git` entry), or null. */
export async function findRoot(dir: string): Promise<string | null> {
  let cur = path.resolve(dir);
  for (;;) {
    try {
      await fs.lstat(path.join(cur, ".git"));
      return cur;
    } catch {
      // keep looking upward
    }
    const up = path.dirname(cur);
    if (up === cur) return null;
    cur = up;
  }
}

async function signature(root: string, rel: string): Promise<string> {
  try {
    const st = await fs.lstat(path.join(root, rel));
    return `${st.size}:${Math.round(st.mtimeMs)}`;
  } catch {
    return "gone";
  }
}

/** The paths of `git status --porcelain=v2 -z` output and the HEAD commit from its header. */
export function parseStatus(out: string): { oid: string; paths: string[] } | null {
  let oid = "";
  const paths: string[] = [];
  for (const rec of out.split("\0")) {
    if (rec === "") continue;
    if (rec.startsWith("# branch.oid ")) {
      const v = rec.slice("# branch.oid ".length);
      oid = v === "(initial)" ? "" : v;
    } else if (rec.startsWith("1 ")) paths.push(rec.split(" ").slice(8).join(" "));
    else if (rec.startsWith("u ")) paths.push(rec.split(" ").slice(10).join(" "));
    else if (rec.startsWith("? ")) paths.push(rec.slice(2));
    if (paths.length > MAX_LISTED) return null;
  }
  return { oid, paths };
}

async function snapshot(git: Git, root: string): Promise<Snapshot | null> {
  const out = await git(["status", "--porcelain=v2", "--branch", "-z", "--no-renames", "--untracked-files=all"], root);
  const parsed = out === null ? null : parseStatus(out);
  if (!parsed) return null;
  const dirty = new Map<string, string>();
  for (const rel of parsed.paths) dirty.set(rel, await signature(root, rel));
  return { oid: parsed.oid, dirty };
}

interface Pending {
  root: string;
  before: Snapshot;
  /** Another command ran in this checkout at the same time. */
  overlapped: boolean;
}

export class ShellChangeCapture {
  private readonly pending = new Map<string, Pending>();
  private readonly cwd: string;
  private readonly git: Git;

  constructor(cwd: string, git: Git = runGit) {
    this.cwd = cwd;
    this.git = git;
  }

  /** Before a command runs. Never throws; a command with no snapshot is simply not tracked. */
  async begin(id: string): Promise<void> {
    try {
      const root = await findRoot(this.cwd);
      if (!root) return;
      const before = await snapshot(this.git, root);
      if (!before) return;
      let overlapped = false;
      for (const other of this.pending.values()) {
        if (other.root === root) {
          other.overlapped = true;
          overlapped = true;
        }
      }
      this.pending.set(id, { root, before, overlapped });
    } catch {
      // not tracked
    }
  }

  /** After the command: the absolute paths it changed (forward slashes), possibly none. Never throws. */
  async finish(id: string): Promise<string[]> {
    const p = this.pending.get(id);
    if (!p) return [];
    this.pending.delete(id);
    try {
      if (p.overlapped) return [];
      const after = await snapshot(this.git, p.root);
      if (!after) return [];
      const changed = new Set<string>();
      for (const [rel, sig] of after.dirty) if (p.before.dirty.get(rel) !== sig) changed.add(rel);
      for (const [rel, was] of p.before.dirty) if (!after.dirty.has(rel) && (await signature(p.root, rel)) !== was) changed.add(rel);
      if (p.before.oid !== after.oid && p.before.oid !== "" && after.oid !== "") {
        const names = await this.git(["diff", "--name-only", "-z", "--no-renames", p.before.oid, after.oid], p.root);
        for (const rel of (names ?? "").split("\0").filter(Boolean).slice(0, MAX_LISTED)) {
          // A file that was already changed and still looks the same was only committed, not moved.
          const was = p.before.dirty.get(rel);
          if (was === undefined || was !== (await signature(p.root, rel))) changed.add(rel);
        }
      }
      const root = p.root.replace(/\\/g, "/").replace(/\/$/, "");
      return [...changed].slice(0, MAX_REPORTED).map((rel) => `${root}/${rel}`);
    } catch {
      return [];
    }
  }

  /** Forget everything in flight (a turn ended or was stopped). */
  clear(): void {
    this.pending.clear();
  }
}
