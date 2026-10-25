// The workflow run as the UI reads it. The host's answer to `workflow_run_get {chatId}` is the
// generated `RunView` binding (host/crates/workflow, types.rs), around the chat's current task,
// with a `workflow_run {chatId}` event after every run write. `showRun` makes the agent's text
// safe to show and keeps each change's own repo and branch for sending back.

import type {
  GuardNoteView,
  OtherTaskView,
  RunChangeView,
  RunPlanView,
  RunTaskView,
  RunView as HostRunView,
} from '../api/generated/workflow/index.ts';
import { inertLines } from '../hosttools/inert.ts';

export type RunTask = RunTaskView;
export type RunPlan = RunPlanView;

/** One change. The text fields are agent text made inert, for display. */
export interface RunChange extends RunChangeView {
  /** The host's own repo and branch, sent back unchanged when the user acts on the change. Never rendered. */
  target: { repo: string; branch: string };
}

/** A task that is not the chat's current one, with its changes that wait on the user. */
export interface OtherTask extends Omit<OtherTaskView, 'waiting'> {
  waiting: RunChange[];
}

export interface RunView {
  task: RunTask | null;
  plan: RunPlan | null;
  changes: RunChange[];
  otherTasks: OtherTask[];
  /** The landing guard's overrides and refusals since the user last typed; commands made inert. */
  guard: GuardNoteView[];
  /** Records for the task board that Helm could not send yet; it retries every minute. */
  boardQueued: number;
}

/** Agent-supplied text (task titles, branches, gate sentences): hidden and direction-changing characters shown as tokens, on one line. */
const inert = (t: string): string => inertLines(t).join(' ');
const inertOrNull = (t: string | null): string | null => (t === null ? null : inert(t));

function showChange(c: RunChangeView): RunChange {
  return {
    ...c,
    repo: inert(c.repo),
    branch: inert(c.branch),
    target: { repo: c.repo, branch: c.branch },
    pr: inertOrNull(c.pr),
    error: inertOrNull(c.error),
    missing: c.missing.map(inert),
  };
}

/** The host's run view, with agent text made inert. */
export function showRun(host: HostRunView): RunView {
  return {
    task: host.task && { ...host.task, title: inert(host.task.title) },
    plan: host.plan && { ...host.plan, missing: inertOrNull(host.plan.missing) },
    changes: host.changes.map(showChange),
    otherTasks: host.otherTasks.map((o) => ({
      ...o,
      title: inert(o.title),
      parkReason: inertOrNull(o.parkReason),
      waiting: o.waiting.map(showChange),
    })),
    guard: (host.guard ?? []).map((g) => ({ ...g, command: inert(g.command) })),
    boardQueued: host.boardQueued ?? 0,
  };
}
