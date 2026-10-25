// The landing guard, worker side. Before Claude Code runs a Bash, PowerShell or MCP tool call,
// the PreToolUse hook asks the host with one `guard_check` line and waits for its
// `guard_verdict` (worker/PROTOCOL-NOTES.md, "Landing guard"). The host decides everything: it
// answers at once, and lets the call through, in any chat without Orchestrator on and an active
// task, and it judges the command, the overrides and the user's words in those chats.
//
// No answer in time: if the host last said the guard is in force for this chat, the call is
// refused with a note to try again (the guard fails closed where work is at stake); otherwise it
// is let through and a warning is logged.

import { randomUUID } from "node:crypto";
import type { Json } from "../../protocol/types.ts";

/** The callback id the worker registers for the landing guard. */
export const LANDING_GUARD_HOOK_ID = "helm-landing-guard";
/** The tools the guard sees. */
export const LANDING_GUARD_MATCHER = "Bash|PowerShell|mcp__.*";
export const GUARD_TIMEOUT_MS = 10_000;
/** Longer commands are sent cut just past the guard's own limit, which it refuses as too large. */
const MAX_COMMAND_CHARS = 1024 * 1024 + 1;
const MAX_INPUT_CHARS = 256 * 1024;

export const NO_ANSWER =
  "Helm did not answer the landing guard in time, so this call did not run. Try it once more; if this keeps happening, the user can turn the landing guard off in Settings → Orchestrator.";

export interface GuardOptions {
  emit: (line: Json) => void;
  diag: (msg: string) => void;
  timeoutMs?: number;
}

/** One PreToolUse call, as the hook sees it. */
export interface GuardCall {
  tool: string;
  input: Json;
  /** The orchestrator's own call (not a subagent's). */
  root: boolean;
  cwd?: string;
}

const deny = (reason: string): Json => ({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: reason } });

export class LandingGuard {
  private readonly o: Required<GuardOptions>;
  private readonly pending = new Map<string, (verdict: Json) => void>();
  /** What the host said last: the guard is in force for this chat. */
  private active = false;
  private readonly session = randomUUID().slice(0, 8);
  private turn = 0;

  constructor(o: GuardOptions) {
    this.o = { timeoutMs: GUARD_TIMEOUT_MS, ...o };
  }

  /** A turn ended: overrides that last a turn (a resend, an armed MCP call) stop counting. */
  endTurn(): void {
    this.turn += 1;
  }

  /** A `guard_verdict` from the host. False when it answers nothing this worker asked. */
  onVerdict(line: Json): boolean {
    const id = typeof line.id === "string" ? line.id : "";
    const settle = this.pending.get(id);
    if (!settle) return false;
    this.pending.delete(id);
    settle(line);
    return true;
  }

  /** The hook's answer for one call: `{}` lets it through. */
  async decide(call: GuardCall): Promise<Json> {
    const kind = call.tool === "Bash" ? "bash" : call.tool === "PowerShell" ? "powershell" : call.tool.startsWith("mcp__") ? "mcp" : undefined;
    if (!kind) return {};
    const id = randomUUID();
    const line: Json = { type: "guard_check", id, kind, root: call.root, turn: `${this.session}:${this.turn}` };
    if (call.cwd) line.cwd = call.cwd;
    if (kind === "mcp") {
      line.tool = call.tool;
      const text = JSON.stringify(call.input ?? {});
      // An input too large to send is judged as nothing; the host still sees the tool's name.
      line.input = text.length <= MAX_INPUT_CHARS ? call.input : {};
    } else {
      const command = typeof call.input.command === "string" ? call.input.command : "";
      line.command = command.slice(0, MAX_COMMAND_CHARS);
    }
    const verdict = await new Promise<Json | undefined>((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve(undefined);
      }, this.o.timeoutMs);
      this.pending.set(id, (v) => {
        clearTimeout(timer);
        resolve(v);
      });
      this.o.emit(line);
    });
    if (!verdict) {
      if (this.active) return deny(NO_ANSWER);
      this.o.diag(`landing guard: no answer from Helm within ${this.o.timeoutMs} ms; the ${call.tool} call was let through`);
      return {};
    }
    this.active = verdict.active === true;
    if (verdict.allow === true) {
      return typeof verdict.context === "string" && verdict.context ? { hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: verdict.context } } : {};
    }
    return deny(typeof verdict.reason === "string" && verdict.reason ? verdict.reason : "Helm's landing guard refused this call.");
  }

  /** The worker is going away: let anything still waiting through rather than hang the CLI. */
  close(): void {
    for (const settle of this.pending.values()) settle({ allow: true, active: this.active });
    this.pending.clear();
  }
}
