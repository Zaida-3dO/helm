// The orchestrator workflow's dispatch and edit gates, asked of the host before an agent spawn or
// a main-thread file edit.
//
// The hook is registered only when the launch carries HELM_WORKFLOW_GATES=1, which the host sets
// for a chat whose Orchestrator may be on; every other chat launches exactly as before. The host
// decides from its own records and answers allow wherever no workflow task is active. These gates
// are soft: a call the host cannot answer in time, or any other failure, is let through.

import type { RequestChannel } from "../../hosttools/channel.ts";
import type { Json } from "../../protocol/types.ts";

export const WORKFLOW_GATE_HOOK_ID = "helm-workflow-gates";
export const GATES_ENV = "HELM_WORKFLOW_GATES";
export const GATE_OPERATION = "app_workflow_gate";
export const SPAWN_TOOLS = ["Agent", "Task"];
export const EDIT_TOOLS = ["Write", "Edit", "MultiEdit", "NotebookEdit"];
export const GATE_TIMEOUT_MS = 5_000;

export function gatesWanted(env: Record<string, string | undefined>): boolean {
  return env[GATES_ENV] === "1";
}

/** The PreToolUse registration for the gated tools. */
export function gateHook(): Json {
  return { matcher: [...SPAWN_TOOLS, ...EDIT_TOOLS].join("|"), hookCallbackIds: [WORKFLOW_GATE_HOOK_ID] };
}

function obj(v: unknown): Json {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Json) : {};
}

/** Only what the host decides on: never a prompt or a file's content. */
function slim(tool: string, input: Json): Json {
  const keep = SPAWN_TOOLS.includes(tool) ? ["subagent_type", "isolation", "run_in_background"] : ["file_path", "notebook_path"];
  const out: Json = {};
  for (const k of keep) if (input[k] !== undefined) out[k] = input[k];
  return out;
}

/** The hook's answer for one PreToolUse input: `{}` lets the call through. */
export async function decideGate(channel: Pick<RequestChannel, "call">, input: Json, child: boolean): Promise<Json> {
  const tool = String(input.tool_name ?? "");
  const spawn = SPAWN_TOOLS.includes(tool);
  if (!spawn && !(EDIT_TOOLS.includes(tool) && !child)) return {};
  const args: Json = { tool, input: slim(tool, obj(input.tool_input)), child };
  if (typeof input.cwd === "string" && input.cwd) args.cwd = input.cwd;
  try {
    const r = await channel.call(GATE_OPERATION, args, { limitBytes: 32 * 1024, timeoutMs: GATE_TIMEOUT_MS, mutating: false, caller: { child: false } });
    const res = r.ok ? obj(r.result) : {};
    if (res.allow === false && typeof res.reason === "string" && res.reason) {
      return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny", permissionDecisionReason: res.reason } };
    }
  } catch {
    // Soft: an unanswered gate lets the call through.
  }
  return {};
}
