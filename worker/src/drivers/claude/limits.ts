// Turn and context limits for sub-agents, counted from what the worker already sees.
//
// A sub-agent's turns are its tool calls, counted from the PreToolUse hook (the hook input names
// the sub-agent by `agent_id`). Its context size is read from the usage on its latest assistant
// record (`parent_tool_use_id` set): input plus cache-read plus cache-write tokens. Near the soft
// limit the hook adds a note telling the sub-agent to write a short hand-off and stop; past the
// hard limit (30% further on) every further tool call is refused with the same instruction, when
// the launch asks for that (`hardStop`; otherwise the sub-agent is only reminded). The refusal is
// soft: the sub-agent's reply is still its way out, and the chat's own agent is never counted or
// refused. Limits off (soft turns 0) registers nothing; the host sets it only for chats the
// user's scope setting covers.

import type { Json } from "../../protocol/types.ts";

export const BUDGET_HOOK_ID = "helm-subagent-budget";
// Context size at which a sub-agent is told to wrap up.
export const SOFT_CONTEXT_TOKENS = 300_000;
const HARD_FACTOR = 1.3;
/// After the soft limit, remind every this many calls.
const REMIND_EVERY = 5;
const MAX_TRACKED = 2_000;

/** Added to the chat's own agent's instructions so it briefs sub-agents accordingly. */
export function subagentRule(softTurns: number): string {
  return `Sub-agents: each one is asked to stay within about ${softTurns} tool calls and about 300k tokens of context. A sub-agent that reaches the limit writes a short hand-off (done, left, key files, open problems) and stops; start a fresh one from the original brief plus that hand-off. Ask sub-agents for short final reports (a few lines: what changed, where, what failed) and to put any detail in a file you can read if needed, so your own context stays small.`;
}

const START_NOTE =
  "Reporting: when you finish, reply with a short report (a few lines: what you did, where, anything that failed or needs a decision). Put long detail in a file and give its path.";

function wrapUpNote(why: string): string {
  return `You have used ${why}. Finish your current step, then write a short hand-off as your final reply and stop: what is done, what is left, the key files and branches, and any open problems. Do not start new work.`;
}

export type BudgetDecision = { kind: "allow" } | { kind: "note"; text: string } | { kind: "deny"; text: string };

export interface BudgetOptions {
  softTurns: number;
  softContext?: number;
  /** Refuse tool calls past the hard limit. Off: past it the sub-agent is only reminded. */
  hardStop?: boolean;
}

export class SubagentBudget {
  readonly softTurns: number;
  private readonly softContext: number;
  private readonly hardStop: boolean;
  /** Tool calls seen per sub-agent key. */
  private readonly turns = new Map<string, number>();
  /** Tool-use id → the Agent call that owns the sub-agent making it. */
  private readonly toolOwner = new Map<string, string>();
  /** Agent call id → latest context size. */
  private readonly context = new Map<string, number>();
  /** Calls at which a hand-off note was last sent. */
  private readonly noted = new Map<string, number>();

  constructor(o: BudgetOptions) {
    this.softTurns = o.softTurns;
    this.softContext = o.softContext ?? SOFT_CONTEXT_TOKENS;
    this.hardStop = o.hardStop === true;
  }

  get enabled(): boolean {
    return this.softTurns > 0;
  }

  /** An assistant record: remember who owns its tool calls and how big its context is. */
  noteAssistant(rec: Json): void {
    if (!this.enabled || rec.type !== "assistant" || typeof rec.parent_tool_use_id !== "string") return;
    const owner = rec.parent_tool_use_id;
    const message = (rec.message ?? {}) as Json;
    const u = (message.usage ?? {}) as Json;
    const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : 0);
    const size = n(u.input_tokens) + n(u.cache_read_input_tokens) + n(u.cache_creation_input_tokens);
    if (size > 0) this.context.set(owner, size);
    for (const b of Array.isArray(message.content) ? message.content : []) {
      const block = (b ?? {}) as Json;
      if (block.type === "tool_use" && typeof block.id === "string") this.toolOwner.set(block.id, owner);
    }
    trim(this.toolOwner);
    trim(this.context);
  }

  /** A PreToolUse hook input. The chat's own agent is always allowed and never counted. */
  decide(input: Json, toolUseId: string): BudgetDecision {
    const agentId = typeof input.agent_id === "string" ? input.agent_id : "";
    const owner = this.toolOwner.get(toolUseId);
    if (!this.enabled || (agentId === "" && owner === undefined)) return { kind: "allow" };
    const key = agentId || owner || "";
    const used = (this.turns.get(key) ?? 0) + 1;
    this.turns.set(key, used);
    trim(this.turns);
    const size = owner === undefined ? 0 : (this.context.get(owner) ?? 0);
    const hardTurns = Math.ceil(this.softTurns * HARD_FACTOR);
    const hardContext = Math.ceil(this.softContext * HARD_FACTOR);
    const past = used > hardTurns || size > hardContext;
    if (past && this.hardStop) {
      return { kind: "deny", text: `${wrapUpNote(used > hardTurns ? `its ${hardTurns} tool calls` : `its context budget`)} Tool calls are refused from here; your reply is the way out.` };
    }
    if (past || used >= this.softTurns || size >= this.softContext) {
      const last = this.noted.get(key);
      if (last === undefined || used - last >= REMIND_EVERY) {
        this.noted.set(key, used);
        trim(this.noted);
        return { kind: "note", text: wrapUpNote(used >= this.softTurns ? `${used} of about ${this.softTurns} tool calls` : `about ${Math.round(size / 1000)}k tokens of context`) };
      }
      return { kind: "allow" };
    }
    if (used === 1) return { kind: "note", text: START_NOTE };
    return { kind: "allow" };
  }
}

function trim<V>(m: Map<string, V>): void {
  while (m.size > MAX_TRACKED) {
    const first = m.keys().next();
    if (first.done) return;
    m.delete(first.value);
  }
}
