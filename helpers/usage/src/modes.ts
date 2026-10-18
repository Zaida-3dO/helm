// Request modes: how many model requests ran
// Fast, Standard or with no recorded mode, counted from the native transcripts of the chats Helm
// itself started. Only files modified at or after `since` are read, a record over 4 MiB is
// skipped and counted, and a request is counted once (by message id, then request id, then
// record id).

import fs from "node:fs";
import path from "node:path";

export interface ModesJob {
  /** The claude config folder's `projects` directory. */
  projectsDir: string;
  /** The chats Helm started: the native session and the folder it ran in. */
  sessions: Array<{ sessionId: string; cwd: string }>;
  /** Epoch milliseconds, inclusive start and exclusive end. */
  since: number;
  until: number;
}

export interface ModesResult {
  fast: number;
  standard: number;
  unknown: number;
  /** Transcript files read. */
  files: number;
  /** Counters for the coverage note: lines that were not JSON, records over the size cap. */
  malformed: number;
  oversized: number;
}

export const MAX_RECORD_BYTES = 4 * 1024 * 1024;
export const MAX_FILES = 5_000;

/** How Claude Code names the folder for a working directory: every other character becomes "-". */
export function encodeCwd(cwd: string): string {
  return cwd.replace(/[^a-zA-Z0-9]/g, "-");
}

export type Mode = "fast" | "standard" | "unknown";

/** fast/priority/ultrafast are fast; standard/default/flex/batch are standard; the rest unknown. */
export function classify(speed: unknown, tier: unknown): Mode {
  for (const v of [speed, tier]) {
    if (typeof v !== "string") continue;
    const s = v.toLowerCase();
    if (s === "fast" || s === "priority" || s === "ultrafast") return "fast";
    if (s === "standard" || s === "default" || s === "flex" || s === "batch") return "standard";
  }
  return "unknown";
}

type Json = Record<string, unknown>;

function transcriptFiles(job: ModesJob): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const s of job.sessions) {
    if (!/^[0-9a-fA-F-]{8,64}$/.test(s.sessionId) || seen.has(s.sessionId)) continue;
    seen.add(s.sessionId);
    const dir = path.join(job.projectsDir, encodeCwd(s.cwd));
    const main = path.join(dir, `${s.sessionId}.jsonl`);
    out.push(main);
    // The sub-agent transcripts of this session sit beside it.
    const subs = path.join(dir, s.sessionId, "subagents");
    try {
      for (const f of fs.readdirSync(subs)) if (f.endsWith(".jsonl")) out.push(path.join(subs, f));
    } catch {
      // No sub-agents.
    }
    if (out.length >= MAX_FILES) break;
  }
  return out;
}

export function countModes(job: ModesJob): ModesResult {
  const r: ModesResult = { fast: 0, standard: 0, unknown: 0, files: 0, malformed: 0, oversized: 0 };
  const counted = new Set<string>();
  for (const file of transcriptFiles(job)) {
    let text: string;
    try {
      const st = fs.statSync(file);
      if (!st.isFile() || st.mtimeMs < job.since) continue;
      text = fs.readFileSync(file, "utf8");
    } catch {
      continue;
    }
    r.files++;
    for (const line of text.split("\n")) {
      if (!line.includes('"assistant"')) continue;
      if (line.length > MAX_RECORD_BYTES) {
        r.oversized++;
        continue;
      }
      let rec: Json;
      try {
        rec = JSON.parse(line) as Json;
      } catch {
        r.malformed++;
        continue;
      }
      if (rec.type !== "assistant") continue;
      const ts = typeof rec.timestamp === "string" ? Date.parse(rec.timestamp) : NaN;
      if (Number.isNaN(ts) || ts < job.since || ts >= job.until) continue;
      const msg = (rec.message ?? {}) as Json;
      const usage = msg.usage;
      if (!usage || typeof usage !== "object") continue;
      const key = String(msg.id ?? rec.requestId ?? rec.uuid ?? "");
      if (key) {
        if (counted.has(key)) continue;
        counted.add(key);
      }
      const u = usage as Json;
      r[classify(u.speed, u.service_tier)]++;
    }
  }
  return r;
}
