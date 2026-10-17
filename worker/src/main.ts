// Worker launcher: `node <launcher> <launch.json>`.
// stdin: host commands. stdout: events and responses. stderr: diagnostics only.

import fs from "node:fs";
import { HostChannel } from "./protocol/channel.ts";
import { MAX_RECORD_BYTES } from "./protocol/lines.ts";
import { serve } from "./protocol/server.ts";
import type { Launch } from "./protocol/types.ts";
import { ClaudeDriver } from "./drivers/claude/driver.ts";

const STDERR_CAP = 16 * 1024;
let stderrUsed = 0;
function diag(msg: string): void {
  // The host keeps 16 KB of stderr; stay well-behaved and stop writing past it.
  const line = `[helm-worker] ${msg}\n`;
  if (stderrUsed >= STDERR_CAP) return;
  stderrUsed += Buffer.byteLength(line);
  process.stderr.write(line);
}

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
  const driver = new ClaudeDriver(launch, channel, { env: process.env, diag, onFatal: exit });
  serve({ input: process.stdin, channel, driver, diag, exit, maxRecordBytes: limit });
}

main();
