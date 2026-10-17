// Who made a host-tool call: the chat's own agent or a subagent.
//
// Claude Code's `mcp_message` carries no tool-use id, so the worker registers a PreToolUse hook
// for the helm server's tools: the hook record names the tool, its input, its tool-use id and,
// for a subagent, an agent id. A `tools/call` is matched to the oldest unclaimed record with the
// same tool and the same input. Records expire after a few seconds and are cleared at turn end,
// so a hook whose call never came cannot be borrowed later. A call that cannot be attributed in
// time is marked `unattributed`: the server sends no host-tool call for it, and it counts as a
// child's for MCP tools, so a gap never widens what a call may do.

import type { Json } from "../protocol/types.ts";
import type { CallerInfo } from "./server.ts";

export const ATTRIBUTION_WAIT_MS = 1_000;
/** How long a hook record waits for its call. */
export const RECORD_TTL_MS = 5_000;
const KEEP = 64;

function canonical(v: unknown): string {
  if (Array.isArray(v)) return `[${v.map(canonical).join(",")}]`;
  if (v && typeof v === "object") {
    const o = v as Json;
    return `{${Object.keys(o)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonical(o[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(v ?? null);
}

interface Seen {
  key: string;
  caller: CallerInfo;
  at: number;
}

export class Attribution {
  private seen: Seen[] = [];
  private waiters: { key: string; resolve: (c: CallerInfo) => void }[] = [];
  private readonly prefix: string;
  private readonly now: () => number;

  /** `prefix`: the CLI's name prefix for the helm server's tools (`mcp__helm__`). */
  constructor(prefix: string, now: () => number = Date.now) {
    this.prefix = prefix;
    this.now = now;
  }

  /** Forget every unclaimed record (at turn end). */
  clear(): void {
    this.seen = [];
  }

  private prune(): void {
    const cutoff = this.now() - RECORD_TTL_MS;
    this.seen = this.seen.filter((s) => s.at > cutoff);
  }

  /** A PreToolUse hook input. Ignores tools that are not the helm server's. */
  noteHook(input: Json): void {
    const full = typeof input.tool_name === "string" ? input.tool_name : "";
    if (!full.startsWith(this.prefix)) return;
    const key = `${full.slice(this.prefix.length)}\u0000${canonical(input.tool_input ?? {})}`;
    const caller: CallerInfo = { child: typeof input.agent_id === "string" && input.agent_id !== "" };
    if (typeof input.tool_use_id === "string" && input.tool_use_id) caller.toolCallId = input.tool_use_id;
    const w = this.waiters.findIndex((x) => x.key === key);
    if (w >= 0) {
      const [hit] = this.waiters.splice(w, 1);
      hit.resolve(caller);
      return;
    }
    this.prune();
    this.seen.push({ key, caller, at: this.now() });
    if (this.seen.length > KEEP) this.seen.shift();
  }

  /** The caller for a `tools/call`, waiting up to `waitMs` for its hook record. */
  take(name: string, args: unknown, waitMs = ATTRIBUTION_WAIT_MS): Promise<CallerInfo> {
    const key = `${name}\u0000${canonical(args ?? {})}`;
    this.prune();
    const same = this.seen.filter((s) => s.key === key);
    if (same.length > 0) {
      // Records that disagree (a parent's and a child's with the same input): the narrower wins,
      // and the child's record (with its own tool-use id) is the one used up.
      const pick = same.find((s) => s.caller.child) ?? same[0];
      this.seen.splice(this.seen.indexOf(pick), 1);
      return Promise.resolve(pick.caller);
    }
    return new Promise((resolve) => {
      const entry = { key, resolve };
      this.waiters.push(entry);
      setTimeout(() => {
        const at = this.waiters.indexOf(entry);
        if (at < 0) return;
        this.waiters.splice(at, 1);
        resolve({ child: true, unattributed: true });
      }, waitMs);
    });
  }
}
