// The control reader: reads plan usage from the unmodified `claude` binary
// and from nothing else.
//
// It starts the binary in stream-json print mode with an empty strict MCP config and no setting
// sources, sends the `initialize`, version and `get_usage` control requests, reads the answer and
// exits. **No user message is ever written**, so no model call is made. Helm itself makes no
// network call, never reads a token store and never calls any `/api/oauth/*` endpoint (CLAUDE.md
// hard rule 2). The control request is experimental upstream, so an old or different binary that
// does not answer is a reported state ("unsupported"), not a crash.

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { buildClaudeEnv } from "../../../worker/src/drivers/claude/env.ts";
import { resolveClaude } from "../../../worker/src/drivers/claude/resolve.ts";
import { normaliseControl, type ControlReading } from "./normalise.ts";

export interface ControlJob {
  /** The claude executable and any leading arguments; resolved like the worker's when absent. */
  command?: string;
  commandArgs?: string[];
  /** Milliseconds to wait for the answer; default 60 s. */
  timeoutMs?: number;
}

export type ControlResult =
  | { ok: true; reading: ControlReading; version: string | null }
  | { ok: false; reason: "no_binary" | "unsupported" | "no_limits" | "timeout" | "error"; message: string; version: string | null };

export const EMPTY_MCP = '{"mcpServers":{}}';
export const DEFAULT_TIMEOUT_MS = 60_000;

export function controlArgs(extra: string[] = []): string[] {
  return [
    ...extra,
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--strict-mcp-config",
    "--mcp-config",
    EMPTY_MCP,
    "--setting-sources",
    "",
    "--no-session-persistence",
  ];
}

/** The requests written to the binary: control requests only, never a `user` line. */
export function controlRequests(): string[] {
  const line = (request_id: string, request: Record<string, unknown>) =>
    JSON.stringify({ type: "control_request", request_id, request }) + "\n";
  return [
    line("init", { subtype: "initialize" }),
    line("ver", { subtype: "get_binary_version" }),
    line("usage", { subtype: "get_usage", skip_behaviors: true }),
  ];
}

type Json = Record<string, unknown>;

export function readControlUsage(job: ControlJob = {}): Promise<ControlResult> {
  return new Promise((resolve) => {
    let command = job.command;
    if (!command) {
      try {
        command = resolveClaude({ env: process.env }).path;
      } catch (e) {
        resolve({ ok: false, reason: "no_binary", message: (e as Error).message, version: null });
        return;
      }
    }
    // A neutral folder, so no project hooks or settings are picked up.
    const cwd = fs.mkdtempSync(path.join(os.tmpdir(), "helm-usage-"));
    let version: string | null = null;
    let done = false;
    let buf = "";
    let child: ReturnType<typeof spawn>;
    try {
      child = spawn(command, controlArgs(job.commandArgs), {
        cwd,
        env: buildClaudeEnv(process.env, {}),
        stdio: ["pipe", "pipe", "ignore"],
        windowsHide: true,
      });
    } catch (e) {
      fs.rmSync(cwd, { recursive: true, force: true });
      resolve({ ok: false, reason: "error", message: (e as Error).message, version: null });
      return;
    }
    // The folder is the child's working directory: it is removed once the child has gone, and
    // the answer waits for that (the helper exits right after answering).
    const finish = (r: ControlResult) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      try {
        child.stdin?.end();
      } catch {
        // The child is already gone.
      }
      let cleaned = false;
      const cleanup = () => {
        if (cleaned) return;
        cleaned = true;
        try {
          fs.rmSync(cwd, { recursive: true, force: true });
        } catch {
          // Best effort; the folder is empty.
        }
        resolve(r);
      };
      child.once("close", cleanup);
      child.kill();
      setTimeout(cleanup, 2_000).unref();
    };
    const timer = setTimeout(
      () => finish({ ok: false, reason: "timeout", message: "The claude binary did not answer the usage request in time.", version }),
      job.timeoutMs ?? DEFAULT_TIMEOUT_MS,
    );
    child.on("error", (e) => finish({ ok: false, reason: "error", message: e.message, version }));
    child.on("close", () =>
      finish({ ok: false, reason: "unsupported", message: "The claude binary exited without answering the usage request.", version }),
    );
    child.stdin?.on("error", () => {
      // A binary that exits early closes the pipe; the close handler reports it.
    });
    child.stdout?.setEncoding("utf8");
    child.stdout?.on("data", (d: string) => {
      buf += d;
      let nl: number;
      while ((nl = buf.indexOf("\n")) !== -1) {
        const line = buf.slice(0, nl).trim();
        buf = buf.slice(nl + 1);
        if (!line.startsWith("{")) continue;
        let rec: Json;
        try {
          rec = JSON.parse(line) as Json;
        } catch {
          continue;
        }
        if (rec.type !== "control_response") continue;
        const resp = (rec.response ?? {}) as Json;
        const body = (resp.response ?? {}) as Json;
        if (resp.request_id === "ver" && resp.subtype === "success" && typeof body.version === "string") version = body.version;
        if (resp.request_id !== "usage") continue;
        if (resp.subtype !== "success") {
          finish({ ok: false, reason: "unsupported", message: String(resp.error ?? "The binary refused the usage request."), version });
          return;
        }
        const reading = normaliseControl(body);
        if (!reading) {
          finish({ ok: false, reason: "no_limits", message: "The binary reports no plan limits for this sign-in.", version });
          return;
        }
        finish({ ok: true, reading, version });
        return;
      }
    });
    for (const l of controlRequests()) child.stdin?.write(l);
  });
}
