// Worker launcher: `node <launcher> <launch.json>`.
// stdin: host commands. stdout: events and responses. stderr: diagnostics only.

import fs from "node:fs";
import { HostChannel } from "./protocol/channel.ts";
import { MAX_RECORD_BYTES } from "./protocol/lines.ts";
import { serve } from "./protocol/server.ts";
import { markLaunch } from "./protocol/timing.ts";
import type { Launch } from "./protocol/types.ts";
import { ClaudeDriver } from "./drivers/claude/driver.ts";
import { takeClaudeToken, takeLocalKey } from "./drivers/claude/env.ts";

const STDERR_CAP = 16 * 1024;
let stderrUsed = 0;
function diag(msg: string): void {
  // The host keeps 16 KB of stderr; stay well-behaved and stop writing past it.
  const line = `[helm-worker] ${msg}\n`;
  if (stderrUsed >= STDERR_CAP) return;
  stderrUsed += Buffer.byteLength(line);
  process.stderr.write(line);
}

/** A line that must reach the host whatever was written before: the host keeps only the tail, so
 *  the last words of a dying worker matter more than the cap. */
function lastWords(msg: string): void {
  try {
    process.stderr.write(`[helm-worker] ${msg}\n`);
  } catch {
    // stderr is gone: nothing more can be said.
  }
}

// Anything thrown outside a handler would otherwise end the worker with Node's own report, which
// the host may not see whole. Say what happened, then end with code 1.
process.on("uncaughtException", (e) => {
  lastWords(`uncaught exception: ${e instanceof Error ? (e.stack ?? e.message) : String(e)}`);
  process.exit(1);
});
process.on("unhandledRejection", (e) => {
  lastWords(`unhandled rejection: ${e instanceof Error ? (e.stack ?? e.message) : String(e)}`);
  process.exit(1);
});
// Written synchronously as the process ends, so an exit with no line at all means the worker was
// ended from outside.
process.on("exit", (code) => lastWords(`exiting with code ${code}`));

function readLaunch(file: string | undefined): Launch {
  if (!file) throw new Error("usage: node main.ts <launch.json>");
  const raw = fs.readFileSync(file, "utf8");
  const v = JSON.parse(raw) as Launch;
  if (!v || typeof v !== "object" || typeof v.chatId !== "string" || v.chatId === "") {
    throw new Error("launch.json must be an object with a chatId");
  }
  if (v.harness !== undefined && v.harness !== "claude") throw new Error(`unsupported harness ${String(v.harness)}`);
  return v;
}

function main(): void {
  markLaunch("mainStart");
  // Node read its compile cache folder at start-up; nothing this worker starts (claude, MCP
  // servers, their hooks) should write into Helm's folder.
  delete process.env.NODE_COMPILE_CACHE;
  let launch: Launch;
  const early = new HostChannel(process.stdout, diag);
  try {
    launch = readLaunch(process.argv[2]);
  } catch (e) {
    // Fail closed on unreadable configuration.
    early.emit({ type: "worker_error", error: `cannot read launch description: ${(e as Error).message}` });
    void early.flush().then(() => process.exit(2));
    return;
  }
  const limit = launch.limits?.maxRecordBytes ?? MAX_RECORD_BYTES;
  const channel = new HostChannel(process.stdout, diag, limit);
  let exiting = false;
  const exit = (code: number) => {
    if (exiting) return;
    exiting = true;
    void channel.flush().then(() => process.exit(code));
  };
  // A local model server's key arrives in the environment; take it out of there before anything
  // else is started, so no child (claude, MCP servers, hooks) inherits it under that name.
  const localKey = takeLocalKey(process.env);
  // Same for the saved Claude token: only the claude child ever gets it, as its own sign-in.
  const claudeToken = takeClaudeToken(process.env);
  const driver = new ClaudeDriver(launch, channel, { env: process.env, localKey, claudeToken, diag, onFatal: exit });
  serve({ input: process.stdin, channel, driver, diag, exit, maxRecordBytes: limit });
}

main();
