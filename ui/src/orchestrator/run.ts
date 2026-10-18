// The workflow run as the UI reads it: the shape of `app_workflow status` (host/crates/workflow
// actions.rs), served to the UI by `workflow_run_get {chatId}` with a `workflow_run {chatId}`
// event when a run or change row changes. Hand-written until the workflow crate exports a binding;
// every field is read defensively because the op is newer than this file.

import { inertLines } from '../hosttools/inert.ts';

export interface RunTask {
  runId: string;
  title: string;
  /** `active` | `parked` | .... */
  status: string;
  attempt: number;
  visual: boolean;
  itemId: string | null;
  /** Stages the user skipped (`plan`, `planReview`, `review`, `cleanup`, `smoketest`). */
  skipped: string[];
}

export interface RunPlan {
  id: string | null;
  approved: boolean;
  signedOff: boolean;
  rounds: number;
  cap: number;
  /** Why a build would not count yet, in the host's words; null when nothing is missing. */
  missing: string | null;
}

export interface RunChange {
  repo: string;
  branch: string;
  pr: string | null;
  /** `open` | `landing_draft` | `pending_land` | `landing` | `awaiting_merge` | `land_failed` | `handed_off` | `landed` | `flipped` | `dropped` | `declined`. */
  state: string;
  failedMode: string | null;
  tip: string | null;
  /** The review's approval holds at the change's latest commit. */
  approved: boolean;
  reviewRounds: number;
  reviewCap: number;
  smoketestRounds: number;
  smoketestCap: number;
  error: string | null;
  /** What the change still needs before it can be handed off, in the host's words. */
  missing: string[];
}

export interface OtherTask {
  runId: string;
  title: string;
  status: string;
  parkReason: string | null;
}

export interface RunView {
  task: RunTask | null;
  plan: RunPlan | null;
  changes: RunChange[];
  otherTasks: OtherTask[];
}

const str = (v: unknown): string | null => (typeof v === 'string' ? v : null);
/** Agent-supplied text (task titles, branches, gate sentences): hidden and direction-changing characters shown as tokens, on one line. */
const inert = (v: unknown): string | null => {
  const t = str(v);
  return t === null ? null : inertLines(t).join(' ');
};
const num = (v: unknown, d = 0): number => (typeof v === 'number' && Number.isFinite(v) ? v : d);
const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const obj = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' ? (v as Record<string, unknown>) : {});
const strings = (v: unknown): string[] => list(v).filter((x): x is string => typeof x === 'string');

/** Read the host's answer into a RunView, tolerating missing fields. */
export function parseRun(raw: unknown): RunView {
  const r = obj(raw);
  const t = r.task === null || r.task === undefined ? null : obj(r.task);
  const p = r.plan === null || r.plan === undefined ? null : obj(r.plan);
  return {
    task: t && {
      runId: str(t.runId) ?? '',
      title: inert(t.title) ?? 'Task',
      status: str(t.status) ?? 'active',
      attempt: num(t.attempt, 1),
      visual: t.visual === true,
      itemId: str(t.itemId),
      skipped: strings(t.skipped),
    },
    plan: p && {
      id: str(p.id),
      approved: p.approved === true,
      signedOff: p.signedOff === true,
      rounds: num(p.rounds),
      cap: num(p.cap),
      missing: inert(p.missing),
    },
    changes: list(r.changes).map((c) => {
      const o = obj(c);
      return {
        repo: inert(o.repo) ?? '',
        branch: inert(o.branch) ?? '',
        pr: inert(o.pr),
        state: str(o.state) ?? 'open',
        failedMode: str(o.failedMode),
        tip: str(o.tip),
        approved: o.approved === true,
        reviewRounds: num(o.reviewRounds),
        reviewCap: num(o.reviewCap),
        smoketestRounds: num(o.smoketestRounds),
        smoketestCap: num(o.smoketestCap),
        error: inert(o.error),
        missing: strings(o.missing).map((m) => inertLines(m).join(' ')),
      };
    }),
    otherTasks: list(r.otherTasks).map((o) => {
      const x = obj(o);
      return { runId: str(x.runId) ?? '', title: inert(x.title) ?? 'Task', status: str(x.status) ?? 'parked', parkReason: inert(x.parkReason) };
    }),
  };
}
