// Fake Claude Code CLI.
// Usage: node fake-claude.ts <fixture.json> [the real CLI's flags, which are logged and mostly ignored]
//
// It speaks stream-json on stdin/stdout and replays a scripted fixture. It answers the control
// requests a real CLI answers (initialize, set_model, apply_flag_settings, set_permission_mode,
// interrupt) and, when started with --replay-user-messages, echoes user messages (deduplicated
// by uuid, as the real CLI does). Everything it receives is appended to $FAKE_CLAUDE_LOG.
//
// Fixture: { "description", "replay"?: bool, "autoInterrupt"?: bool, "steps": Step[] }
//   { "await": <partial record>, "as"?: name, "timeoutMs"?: n }  wait for a matching stdin record
//   { "emit": <record> }                    write one record; "{{name.path}}" strings are filled in
//                                           from saved records, "{{repeat:x:N}}" becomes N x's
//   { "emitRaw": text, "newline"?: bool }   write raw text (torn or non-JSON lines)
//   { "emitOversize": bytes }               write one valid JSON record of about this many bytes
//   { "sleep": ms }
//   { "exit": code }                        exit at once (a crash, mid-turn if you like)
//   { "hang": true }                        stop answering anything (a CLI that never settles)

import fs from "node:fs";

type Json = Record<string, unknown>;
type Step = Json;

const fixturePath = process.argv[2];
if (!fixturePath) {
  process.stderr.write("fake-claude: fixture path required\n");
  process.exit(64);
}
const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf8")) as { steps: Step[]; replay?: boolean; autoInterrupt?: boolean };
const flags = process.argv.slice(3);
const logPath = process.env.FAKE_CLAUDE_LOG;

function log(entry: Json): void {
  if (logPath) fs.appendFileSync(logPath, JSON.stringify(entry) + "\n");
}
log({ kind: "start", argv: flags, envKeys: Object.keys(process.env).sort(), pid: process.pid });

const replay = fixture.replay !== false && flags.includes("--replay-user-messages");
const autoInterrupt = fixture.autoInterrupt !== false;
let hung = false;
const seenUuids = new Set<string>();
const inbox: Json[] = [];
let waiter: { match: Json; resolve: (r: Json) => void } | undefined;
const saved: Record<string, Json> = {};

function out(rec: Json): void {
  process.stdout.write(JSON.stringify(rec) + "\n");
}

function matches(rec: unknown, pattern: unknown): boolean {
  if (pattern === null || typeof pattern !== "object") return rec === pattern;
  if (rec === null || typeof rec !== "object") return false;
  for (const [k, v] of Object.entries(pattern as Json)) {
    if (!matches((rec as Json)[k], v)) return false;
  }
  return true;
}

function lookup(path: string): unknown {
  const [name, ...rest] = path.split(".");
  let cur: unknown = saved[name];
  for (const k of rest) cur = cur && typeof cur === "object" ? (cur as Json)[k] : undefined;
  return cur;
}

function fill(v: unknown): unknown {
  if (typeof v === "string") {
    const rep = /^\{\{repeat:(.):(\d+)\}\}$/.exec(v);
    if (rep) return rep[1].repeat(Number(rep[2]));
    const whole = /^\{\{([\w.]+)\}\}$/.exec(v);
    if (whole) return lookup(whole[1]);
    return v.replace(/\{\{([\w.]+)\}\}/g, (_m, p: string) => String(lookup(p) ?? ""));
  }
  if (Array.isArray(v)) return v.map(fill);
  if (v && typeof v === "object") return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, fill(x)]));
  return v;
}

function autoAnswer(rec: Json): void {
  if (rec.type === "user" && replay) {
    const uuid = typeof rec.uuid === "string" ? rec.uuid : undefined;
    if (uuid && seenUuids.has(uuid)) return;
    if (uuid) seenUuids.add(uuid);
    out({ ...rec, isReplay: true });
    return;
  }
  if (rec.type !== "control_request") return;
  const req = (rec.request ?? {}) as Json;
  const ok = (response: Json) => out({ type: "control_response", response: { subtype: "success", request_id: rec.request_id, response } });
  switch (req.subtype) {
    case "initialize":
      return ok({
        commands: [],
        models: [
          { value: "default", displayName: "Default", description: "fake default" },
          { value: "haiku", displayName: "Haiku", description: "fake haiku" },
        ],
      });
    case "set_model":
    case "apply_flag_settings":
    case "set_permission_mode":
      return ok({});
    case "interrupt":
      if (autoInterrupt) ok({});
      return;
    default:
      return;
  }
}

function onRecord(rec: Json): void {
  log({ kind: "in", rec });
  if (hung) return;
  autoAnswer(rec);
  if (waiter && matches(rec, waiter.match)) {
    const w = waiter;
    waiter = undefined;
    w.resolve(rec);
    return;
  }
  inbox.push(rec);
}

function awaitRecord(match: Json, timeoutMs: number): Promise<Json> {
  const i = inbox.findIndex((r) => matches(r, match));
  if (i !== -1) return Promise.resolve(inbox.splice(i, 1)[0]);
  return new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`fake-claude: timed out awaiting ${JSON.stringify(match)}`)), timeoutMs);
    waiter = {
      match,
      resolve: (r) => {
        clearTimeout(t);
        resolve(r);
      },
    };
  });
}

let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk: string) => {
  buf += chunk;
  let nl: number;
  while ((nl = buf.indexOf("\n")) !== -1) {
    const line = buf.slice(0, nl).trim();
    buf = buf.slice(nl + 1);
    if (!line) continue;
    try {
      onRecord(JSON.parse(line) as Json);
    } catch {
      log({ kind: "bad_input", line: line.slice(0, 200) });
    }
  }
});
process.stdin.on("end", () => {
  log({ kind: "stdin_end" });
  if (!hung) process.exit(0);
});

async function run(): Promise<void> {
  for (const step of fixture.steps) {
    if (step.await) {
      const rec = await awaitRecord(step.await as Json, Number(step.timeoutMs ?? 10_000));
      saved.last = rec;
      if (typeof step.as === "string") saved[step.as] = rec;
    } else if (step.emit) {
      out(fill(step.emit) as Json);
    } else if (typeof step.emitRaw === "string") {
      process.stdout.write(step.emitRaw + (step.newline === false ? "" : "\n"));
    } else if (typeof step.emitOversize === "number") {
      const pad = "x".repeat(Math.max(0, step.emitOversize));
      out({ type: "system", subtype: "fake_padding", pad });
    } else if (typeof step.sleep === "number") {
      await new Promise((r) => setTimeout(r, step.sleep as number));
    } else if (typeof step.exit === "number") {
      log({ kind: "exit", code: step.exit });
      // Flush stdout before dying so the worker sees what came before the crash.
      process.stdout.write("", () => process.exit(step.exit as number));
      return;
    } else if (step.hang) {
      hung = true;
      log({ kind: "hang" });
      setInterval(() => {}, 60_000); // keep the process alive until killed
      return;
    }
  }
  log({ kind: "script_done" });
}

run().catch((e: Error) => {
  log({ kind: "script_error", message: e.message });
  process.stderr.write(`${e.message}\n`);
  process.exit(70);
});
