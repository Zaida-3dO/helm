// The send timeline's worker marks (v0.2 latency). Epoch milliseconds for the steps between a
// host send and Claude having the prompt, so a slow first message can be split into Helm's share
// and Claude Code's. Recording a mark never changes behaviour.
//
// - Launch marks (once per worker, first value wins): `workerStart` (the Node process began),
//   `mainStart`, `claudeSpawned`, `initialized`, `versioned`, `ready`. They go out as the
//   `timing` field of the `get_state` response.
// - Prompt marks (per submission id, first value wins): `received` (the host line arrived) and
//   `written` (the prompt reached Claude's stdin). They go out as the `timing` field of that
//   prompt's response; a repeated submission id gets the same marks again.

import { performance } from "node:perf_hooks";

export type Marks = Record<string, number>;

const launch: Marks = { workerStart: Math.round(performance.timeOrigin) };
const prompts = new Map<string, Marks>();
/** The oldest submission's marks are dropped past this, so the table stays small. */
const MAX_PROMPTS = 64;

export function markLaunch(name: string, at: number = Date.now()): void {
  if (launch[name] === undefined) launch[name] = at;
}

export function launchMarks(): Marks {
  return { ...launch };
}

export function markPrompt(submissionId: unknown, name: string, at: number = Date.now()): void {
  if (typeof submissionId !== "string" || !submissionId) return;
  let m = prompts.get(submissionId);
  if (!m) {
    if (prompts.size >= MAX_PROMPTS) prompts.delete(prompts.keys().next().value as string);
    m = {};
    prompts.set(submissionId, m);
  }
  if (m[name] === undefined) m[name] = at;
}

/** A copy of the submission's marks, if it has any. */
export function promptMarks(submissionId: unknown): Marks | undefined {
  if (typeof submissionId !== "string") return undefined;
  const m = prompts.get(submissionId);
  return m ? { ...m } : undefined;
}
