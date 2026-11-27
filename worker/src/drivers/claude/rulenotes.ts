// Rule notes, worker side. When a chat's rules carry moment triggers, the launch gives the worker
// a hook matcher and this hook is registered for it. Before Claude Code runs a matching tool call,
// the PreToolUse hook asks the host with one `rule_check` line and waits for its `rule_verdict`
// (worker/PROTOCOL-NOTES.md, "Rule notes"). The host decides everything: which rules fire on this
// call, whether one of them stops it once, and which notes this chat has not been sent yet.
//
// No answer in time: the call is let through and a line is logged — rule notes only inform, they
// never hold work up. A note counts as shown only when the worker confirms it with `rules_shown`;
// an unacknowledged answer may come again on a later call.

import { randomUUID } from "node:crypto";
import type { Json } from "../../protocol/types.ts";

/** The callback id the worker registers for rule notes. */
export const RULE_NOTES_HOOK_ID = "helm-rule-notes";
export const RULE_NOTE_TIMEOUT_MS = 1_500;
/** Inputs past this size go as their path fields only, so a big Write or Edit still matches its folder. */
const MAX_INPUT_CHARS = 64 * 1024;

export interface RuleNoteOptions {
  emit: (line: Json) => void;
  diag: (msg: string) => void;
  timeoutMs?: number;
}

/** One PreToolUse call, as the hook sees it. */
export interface RuleCall {
  tool: string;
  input: Json;
  cwd?: string;
  /** The chat's own folder, so path checks work when the hook reports no cwd. */
  root?: string;
  /** The sub-agent making the call; its notes and refusals count apart from the root chat's. */
  agentId?: string;
}

const deny = (reason: string): Json => ({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: reason } });
const note = (text: string): Json => ({ hookSpecificOutput: { hookEventName: "PreToolUse", additionalContext: text } });

/** Cut a huge tool input to the fields path triggers read; everything else goes. */
function shrinkInput(input: Json): Json {
  const text = JSON.stringify(input ?? {});
  if (text.length <= MAX_INPUT_CHARS) return input ?? {};
  const kept: Json = {};
  for (const field of ["file_path", "notebook_path", "path"]) {
    if (typeof input?.[field] === "string") kept[field] = input[field];
  }
  if (Array.isArray(input?.paths)) kept.paths = input.paths.filter((p) => typeof p === "string");
  return Object.keys(kept).length > 0 ? kept : {};
}

/** The rule name for a row from its key (`<layer>/<file>.md`); mirrors the host's own stem. */
function keyStem(key: string): string {
  const file = key.split("/").pop() ?? key;
  return file.endsWith(".md") ? file.slice(0, -3) : file;
}

export class RuleNotes {
  private readonly o: Required<RuleNoteOptions>;
  private readonly pending = new Map<string, (verdict: Json | undefined) => void>();
  /** Per-worker id; the epoch is `<session>:<compactions>`, so a new worker or a compaction starts the notes afresh. */
  private readonly session = randomUUID().slice(0, 8);
  private compactions = 0;

  constructor(o: RuleNoteOptions) {
    this.o = { timeoutMs: RULE_NOTE_TIMEOUT_MS, ...o };
  }

  /** A compaction ended: every context of the chat starts over. */
  compacted(): void {
    this.compactions += 1;
  }

  /** A `rule_verdict` from the host. False when it answers nothing this worker asked. */
  onVerdict(line: Json): boolean {
    const id = typeof line.id === "string" ? line.id : "";
    const settle = this.pending.get(id);
    if (!settle) return false;
    this.pending.delete(id);
    settle(line);
    return true;
  }

  /** The hook's answer for one call: `{}` lets it through. */
  async decide(call: RuleCall): Promise<Json> {
    const id = randomUUID();
    const line: Json = { type: "rule_check", id, tool: call.tool, input: shrinkInput(call.input), epoch: `${this.session}:${this.compactions}` };
    if (call.cwd) line.cwd = call.cwd;
    if (call.root) line.root = call.root;
    if (call.agentId) line.agentId = call.agentId;
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
      this.o.diag(`rule notes: no answer from Helm within ${this.o.timeoutMs} ms; the ${call.tool} call was let through`);
      return {};
    }
    if (typeof verdict.stop === "string" && verdict.stop) {
      // A one-time refusal, already spent on the host side for this context.
      return deny(verdict.stop);
    }
    const text = typeof verdict.context === "string" ? verdict.context : "";
    const shown = Array.isArray(verdict.shown) ? (verdict.shown as Json[]) : [];
    if (!text) return {};
    // The note reaches the model through the hook answer; confirm it so the host marks those
    // rules sent for this context. Unacknowledged is safe the other way too: a lost answer may
    // come again, while marking without showing would leave the rule never shown at all.
    const names = shown.map((s) => keyStem(String(s.key ?? ""))).filter(Boolean);
    this.o.emit({
      type: "rules_shown",
      text: names.length > 0 ? `Rules noted on this call: ${names.join(", ")}` : "A rule note went with this call.",
      keys: shown.map((s) => ({ key: String(s.key ?? ""), revision: String(s.revision ?? "") })),
      agentId: call.agentId ?? "root",
    });
    return note(text);
  }

  /** The worker is going away: let anything still waiting through rather than hang the CLI. */
  close(): void {
    for (const settle of this.pending.values()) settle({});
    this.pending.clear();
  }
}
