// stdio transport: one child process, newline-delimited JSON-RPC on stdin/stdout.
// Windows: only real executables are spawned. A `.cmd`/`.bat` command is refused, except npm's
// own shims and package-bin shims, which are rewritten to `node.exe <the shim's target script>`
// once the target is found and checked.

import { spawn, type ChildProcess } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { McpError, type Json, type ResolvedServer } from "./types.ts";
import type { Transport } from "./session.ts";

/** Never handed to an MCP server (as for the claude child). */
const STRIPPED_ENV = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "CLAUDE_CODE_OAUTH_TOKEN", "CLAUDECODE"];

/** Raw stderr kept for the exit message; redacted as a whole, then only the tail is shown. */
const STDERR_KEEP = 8192;

export interface SpawnPlan {
  command: string;
  args: string[];
}

function isShim(cmd: string): boolean {
  return /\.(cmd|bat)$/i.test(cmd);
}

/** Finds a command on PATH the way CreateProcess would, preferring .exe, then .cmd/.bat. */
function onPath(cmd: string, env: NodeJS.ProcessEnv): string | undefined {
  if (path.isAbsolute(cmd) || cmd.includes("/") || cmd.includes("\\")) return fs.existsSync(cmd) ? cmd : undefined;
  const dirs = (env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean);
  const exts = path.extname(cmd) ? [""] : [".exe", ".cmd", ".bat", ""];
  for (const d of dirs) {
    for (const e of exts) {
      const p = path.join(d, cmd + e);
      try {
        if (fs.statSync(p).isFile()) return p;
      } catch {
        /* next */
      }
    }
  }
  return undefined;
}

/** The JS file an npm-generated `.cmd` shim runs, or undefined when it is not such a shim. */
export function shimTarget(shimPath: string): string | undefined {
  let text: string;
  try {
    const st = fs.statSync(shimPath);
    if (!st.isFile() || st.size > 16 * 1024) return undefined;
    text = fs.readFileSync(shimPath, "utf8");
  } catch {
    return undefined;
  }
  const dir = path.dirname(shimPath);
  // npm's own npx.cmd / npm.cmd: they run node_modules\npm\bin\<x>-cli.js beside node.exe.
  const own = /node_modules\\npm\\bin\\(npx|npm)-cli\.js/i.exec(text);
  if (own) {
    const p = path.join(dir, "node_modules", "npm", "bin", `${own[1]}-cli.js`);
    return fs.existsSync(p) ? p : undefined;
  }
  // cmd-shim's package bins: "%_prog%"  "%dp0%\node_modules\pkg\bin\x.js" %*
  const m = /"%~?dp0%?\\([^"%]+\.(?:c|m)?js)"/i.exec(text);
  if (!m) return undefined;
  const target = path.resolve(dir, m[1]!);
  if (!target.startsWith(dir + path.sep)) return undefined;
  return fs.existsSync(target) ? target : undefined;
}

/** What will actually be spawned for `s`, or a public refusal. */
export function planSpawn(s: ResolvedServer, env: NodeJS.ProcessEnv, platform = process.platform, node = process.execPath): SpawnPlan {
  const cmd = s.command ?? "";
  const args = s.args ?? [];
  if (platform !== "win32") return { command: cmd, args };
  const found = onPath(cmd, env);
  if (!found || !isShim(found)) return { command: cmd, args };
  const target = shimTarget(found);
  if (!target) {
    throw new McpError(
      `${s.name}: ${path.basename(found)} is a .cmd or .bat script, which Helm does not run. Use the program's .exe, or node.exe with the script's path.`,
    );
  }
  return { command: node, args: [target, ...args] };
}

export interface StdioOptions {
  cwd?: string;
  env: NodeJS.ProcessEnv;
  maxRecordBytes: number;
  /** Scrubs secrets from the stderr tail before it is cut. */
  redact?: (s: string) => string;
}

export class StdioTransport implements Transport {
  onMessage: (msg: Json) => void = () => {};
  onClose: (why: string) => void = () => {};
  private child: ChildProcess | undefined;
  private buf = "";
  private stderr = "";
  private ended = false;

  private readonly server: ResolvedServer;
  private readonly opts: StdioOptions;

  constructor(server: ResolvedServer, opts: StdioOptions) {
    this.server = server;
    this.opts = opts;
  }

  /** Spawns the child. Throws a public error when the command is refused or cannot start. */
  start(): void {
    const plan = planSpawn(this.server, this.opts.env);
    const env: NodeJS.ProcessEnv = { ...this.opts.env, ...(this.server.env ?? {}) };
    for (const k of STRIPPED_ENV) delete env[k];
    const child = spawn(plan.command, plan.args, {
      cwd: this.opts.cwd,
      env,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
      shell: false,
    });
    this.child = child;
    child.stdout!.setEncoding("utf8");
    child.stdout!.on("data", (c: string) => this.onData(c));
    child.stderr!.setEncoding("utf8");
    child.stderr!.on("data", (c: string) => {
      this.stderr = (this.stderr + c).slice(-STDERR_KEEP);
    });
    child.on("error", (e) => this.end(`${this.server.name} could not start: ${e.message}`));
    child.on("close", (code, sig) => {
      const clean = (this.opts.redact ?? ((s: string) => s))(this.stderr);
      const tail = clean.trim().split("\n").slice(-3).join(" ").slice(-400);
      this.end(`${this.server.name} exited (${sig ?? `code ${code}`})${tail ? `: ${tail}` : ""}.`);
    });
    child.stdin!.on("error", () => {});
  }

  private onData(chunk: string): void {
    this.buf += chunk;
    if (this.buf.length > this.opts.maxRecordBytes && !this.buf.includes("\n")) {
      this.end(`${this.server.name} sent a message larger than 4 MiB.`);
      this.close();
      return;
    }
    let i: number;
    while ((i = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, i).trim();
      this.buf = this.buf.slice(i + 1);
      if (line === "") continue;
      if (line.length > this.opts.maxRecordBytes) {
        this.end(`${this.server.name} sent a message larger than 4 MiB.`);
        this.close();
        return;
      }
      try {
        const v = JSON.parse(line) as unknown;
        for (const m of Array.isArray(v) ? v : [v]) if (m && typeof m === "object") this.onMessage(m as Json);
      } catch {
        // A server that logs to stdout: ignore the line.
      }
    }
  }

  private end(why: string): void {
    if (this.ended) return;
    this.ended = true;
    this.onClose(why);
  }

  async send(msg: Json): Promise<void> {
    const c = this.child;
    if (!c || this.ended || !c.stdin || c.stdin.destroyed) throw new McpError(`${this.server.name} is not running.`);
    await new Promise<void>((resolve, reject) => {
      c.stdin!.write(`${JSON.stringify(msg)}\n`, (e) => (e ? reject(new McpError(`${this.server.name}: ${e.message}`)) : resolve()));
    });
  }

  close(): void {
    this.end("closed");
    const c = this.child;
    if (!c) return;
    try {
      c.stdin?.end();
    } catch {
      /* ignore */
    }
    if (c.exitCode === null && c.signalCode === null) {
      try {
        c.kill();
      } catch {
        /* ignore */
      }
    }
  }
}
