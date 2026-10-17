// Finds the claude executable. Order: HELM_CLAUDE_PATH, then claude(.exe) on PATH,
// then ~/.local/bin/claude(.exe), then the newest VS Code extension's native binary.
// Shell wrapper scripts are refused (they could add unsafe flags; Node also refuses to spawn
// .cmd files without a shell).

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export interface ResolveEnv {
  env: Record<string, string | undefined>;
  platform?: NodeJS.Platform;
  homedir?: string;
}

export interface Resolved {
  path: string;
  source: "HELM_CLAUDE_PATH" | "PATH" | "local-bin" | "vscode-extension" | "launch";
}

const SHIM_EXT = new Set([".cmd", ".bat", ".ps1", ".vbs", ".js", ".sh"]);

export class ClaudeNotFound extends Error {}

export function isShim(p: string): boolean {
  return SHIM_EXT.has(path.extname(p).toLowerCase());
}

/** Rejects a path that is a wrapper script; returns it unchanged otherwise. */
export function assertNotShim(p: string, label: string): string {
  if (isShim(p)) {
    throw new ClaudeNotFound(`${label} points at a wrapper script (${path.basename(p)}); give the real claude executable`);
  }
  return p;
}

function isFile(p: string): boolean {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

/** Compares dotted numeric versions ("2.1.301" > "2.1.295"); non-numeric parts compare as text. */
export function compareVersions(a: string, b: string): number {
  const pa = a.split(/[.-]/);
  const pb = b.split(/[.-]/);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const x = pa[i] ?? "";
    const y = pb[i] ?? "";
    const nx = /^\d+$/.test(x) ? Number(x) : NaN;
    const ny = /^\d+$/.test(y) ? Number(y) : NaN;
    if (!Number.isNaN(nx) && !Number.isNaN(ny)) {
      if (nx !== ny) return nx - ny;
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

export function resolveClaude(opts: ResolveEnv): Resolved {
  const platform = opts.platform ?? process.platform;
  const win = platform === "win32";
  const home = opts.homedir ?? opts.env.USERPROFILE ?? opts.env.HOME ?? os.homedir();
  const exe = win ? "claude.exe" : "claude";

  const override = opts.env.HELM_CLAUDE_PATH;
  if (override && override.trim() !== "") {
    assertNotShim(override, "HELM_CLAUDE_PATH");
    if (!isFile(override)) throw new ClaudeNotFound(`HELM_CLAUDE_PATH does not name a file: ${override}`);
    return { path: override, source: "HELM_CLAUDE_PATH" };
  }

  const pathVar = opts.env.PATH ?? opts.env.Path ?? "";
  for (const dir of pathVar.split(win ? ";" : ":")) {
    if (!dir) continue;
    const candidate = path.join(dir.replace(/^"|"$/g, ""), exe);
    if (isFile(candidate)) return { path: candidate, source: "PATH" };
  }

  const local = path.join(home, ".local", "bin", exe);
  if (isFile(local)) return { path: local, source: "local-bin" };

  const extRoot = path.join(home, ".vscode", "extensions");
  let best: { version: string; file: string } | undefined;
  let entries: string[] = [];
  try {
    entries = fs.readdirSync(extRoot);
  } catch {
    entries = [];
  }
  for (const name of entries) {
    const m = /^anthropic\.claude-code-(\d+(?:\.\d+)*)(?:-.*)?$/.exec(name);
    if (!m) continue;
    const file = path.join(extRoot, name, "resources", "native-binary", exe);
    if (!isFile(file)) continue;
    if (!best || compareVersions(m[1], best.version) > 0) best = { version: m[1], file };
  }
  if (best) return { path: best.file, source: "vscode-extension" };

  throw new ClaudeNotFound(
    "Claude Code executable not found: set HELM_CLAUDE_PATH, put claude.exe on PATH, or install it to ~/.local/bin",
  );
}
