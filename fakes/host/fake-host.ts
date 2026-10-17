// Fake app host: spawns a worker the way the actor does (node <launcher>
// <launch.json>), drives commands over stdin and asserts the protocol on everything the worker
// writes: one JSON object per line, under the record limit, and exactly one response per
// id-bearing command.

import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

export type Json = Record<string, unknown>;

export const DEFAULT_MAX_RECORD = 96 * 1024 * 1024;

export interface FakeHostOptions {
  /** Path to the worker launcher script. */
  launcher: string;
  launch: Json;
  env?: Record<string, string | undefined>;
  cwd?: string;
  maxRecordBytes?: number;
}

export interface Exit {
  code: number | null;
  signal: NodeJS.Signals | null;
}

export class FakeHost {
  readonly events: Json[] = [];
  /** Protocol violations seen on the worker's stdout. Tests assert this stays empty. */
  readonly violations: string[] = [];
  readonly dir: string;
  readonly launchPath: string;
  stderr = "";
  private child: ChildProcess;
  private buf = "";
  private readonly pendingIds = new Map<string, string>(); // id -> command
  private readonly answered = new Map<string, number>();
  private readonly waiters: { pred: (e: Json) => boolean; resolve: (e: Json) => void; from: number }[] = [];
  readonly exited: Promise<Exit>;
  private readonly limit: number;

  constructor(opts: FakeHostOptions) {
    this.limit = opts.maxRecordBytes ?? DEFAULT_MAX_RECORD;
    this.dir = fs.mkdtempSync(path.join(os.tmpdir(), "helm-fh-"));
    this.launchPath = path.join(this.dir, "launch.json");
    fs.writeFileSync(this.launchPath, JSON.stringify(opts.launch));
    this.child = spawn(process.execPath, [opts.launcher, this.launchPath], {
      cwd: opts.cwd ?? this.dir,
      env: (opts.env ?? process.env) as NodeJS.ProcessEnv,
      stdio: ["pipe", "pipe", "pipe"],
      windowsHide: true,
    });
    this.child.stdout!.setEncoding("utf8");
    this.child.stdout!.on("data", (d: string) => this.onData(d));
    this.child.stderr!.setEncoding("utf8");
    this.child.stderr!.on("data", (d: string) => (this.stderr += d));
    this.child.stdin!.on("error", () => {});
    this.exited = new Promise((resolve) => this.child.once("exit", (code, signal) => resolve({ code, signal })));
  }

  get pid(): number | undefined {
    return this.child.pid;
  }

  private onData(d: string): void {
    this.buf += d;
    let nl: number;
    while ((nl = this.buf.indexOf("\n")) !== -1) {
      const line = this.buf.slice(0, nl);
      this.buf = this.buf.slice(nl + 1);
      this.onLine(line);
    }
    if (Buffer.byteLength(this.buf) > this.limit) {
      this.violations.push(`a record grew past ${this.limit} bytes without a newline`);
      this.buf = "";
    }
  }

  private onLine(line: string): void {
    if (Buffer.byteLength(line) > this.limit) this.violations.push(`record over ${this.limit} bytes`);
    let rec: unknown;
    try {
      rec = JSON.parse(line);
    } catch {
      this.violations.push(`non-JSON line on stdout: ${line.slice(0, 120)}`);
      return;
    }
    if (!rec || typeof rec !== "object" || Array.isArray(rec)) {
      this.violations.push(`stdout record is not an object: ${line.slice(0, 120)}`);
      return;
    }
    const e = rec as Json;
    if (typeof e.type !== "string") this.violations.push(`record without a type: ${line.slice(0, 120)}`);
    if (e.type === "response") {
      const id = String(e.id);
      const n = (this.answered.get(id) ?? 0) + 1;
      this.answered.set(id, n);
      if (n > 1) this.violations.push(`second response for id ${id}`);
      const cmd = this.pendingIds.get(id);
      if (cmd === undefined) this.violations.push(`response for an id the host never sent: ${id}`);
      else if (e.command !== cmd) this.violations.push(`response for ${id} names command ${String(e.command)}, expected ${cmd}`);
    }
    this.events.push(e);
    for (const w of [...this.waiters]) {
      if (w.pred(e)) {
        this.waiters.splice(this.waiters.indexOf(w), 1);
        w.resolve(e);
      }
    }
  }

  /** Writes one command line. */
  send(cmd: Json): void {
    if (typeof cmd.id === "string") this.pendingIds.set(cmd.id, String(cmd.type));
    this.child.stdin!.write(JSON.stringify(cmd) + "\n");
  }

  /** Writes raw text to the worker's stdin (for framing tests). */
  sendRaw(text: string): void {
    this.child.stdin!.write(text);
  }

  /** Sends an id-bearing command and resolves with its response. */
  request(cmd: Json, timeoutMs = 10_000): Promise<Json> {
    const id = typeof cmd.id === "string" ? cmd.id : randomUUID();
    const full = { ...cmd, id };
    const p = this.waitFor((e) => e.type === "response" && e.id === id, timeoutMs, 0);
    this.send(full);
    return p;
  }

  /**
   * Resolves with the first event matching pred. By default only events arriving after the call
   * count; pass from=0 to include history.
   */
  waitFor(pred: (e: Json) => boolean, timeoutMs = 10_000, from: number = this.events.length): Promise<Json> {
    const hit = this.events.slice(from).find(pred);
    if (hit) return Promise.resolve(hit);
    return new Promise((resolve, reject) => {
      const t = setTimeout(() => {
        const i = this.waiters.indexOf(w);
        if (i !== -1) this.waiters.splice(i, 1);
        reject(new Error(`timed out after ${timeoutMs} ms; events so far: ${this.events.map((x) => x.type).join(",")}; stderr: ${this.stderr.slice(-500)}`));
      }, timeoutMs);
      const w = {
        pred,
        from,
        resolve: (e: Json) => {
          clearTimeout(t);
          resolve(e);
        },
      };
      this.waiters.push(w);
    });
  }

  waitForType(type: string, timeoutMs = 10_000, from?: number): Promise<Json> {
    return this.waitFor((e) => e.type === type, timeoutMs, from);
  }

  ofType(type: string): Json[] {
    return this.events.filter((e) => e.type === type);
  }

  /** Ids sent with an id but never answered. */
  unanswered(): string[] {
    return [...this.pendingIds.keys()].filter((id) => !this.answered.has(id));
  }

  /** get_state handshake. */
  async ready(timeoutMs = 10_000): Promise<Json> {
    return this.request({ type: "get_state", id: `ready-${randomUUID()}` }, timeoutMs);
  }

  closeStdin(): void {
    this.child.stdin!.end();
  }

  kill(): void {
    if (this.child.exitCode === null) this.child.kill();
  }

  /** Ends the worker if still running and removes the temp folder. */
  async dispose(): Promise<void> {
    if (this.child.exitCode === null && this.child.signalCode === null) {
      try {
        this.send({ type: "shutdown" });
      } catch {
        /* stdin may be gone */
      }
      const t = setTimeout(() => this.kill(), 5000);
      await this.exited;
      clearTimeout(t);
    }
    fs.rmSync(this.dir, { recursive: true, force: true });
  }
}
