// Stage progress, gates and "Needs you" for one orchestrator chat. Pure functions over the host's run view and the saved settings, so node:test can run
// them. The host decides every gate; this only reads what it reports.

import type { WorkflowSettings } from './api.ts';
import type { RunChange, RunView } from './run.ts';

export type StageId = 'plan' | 'planReview' | 'build' | 'review' | 'cleanup' | 'land';
export type StageState = 'done' | 'current' | 'pending' | 'off';

export interface StageChip {
  id: StageId;
  label: string;
  state: StageState;
  /** Short text beside the label: a round count, a state. */
  detail?: string;
}

export const TERMINAL = ['landed', 'flipped', 'dropped', 'declined'];

/** Change state labels. */
export const CHANGE_STATE_LABEL: Record<string, string> = {
  open: 'Open',
  landing_draft: 'Handing off…',
  handed_off: 'Draft PR · your review',
  pending_land: 'Landing…',
  landing: 'Landing…',
  awaiting_merge: 'Awaiting your Merge',
  land_failed: 'Land failed',
  landed: 'Landed',
  flipped: 'Ready for review',
  dropped: 'Dropped',
  declined: 'Merge declined',
};

export const changeLabel = (state: string): string => CHANGE_STATE_LABEL[state] ?? state;

const live = (c: RunChange): boolean => c.state !== 'dropped';

type Skippable = 'plan' | 'planReview' | 'review' | 'cleanup';

/** A stage is off when its setting is off or the user skipped it for this task. */
function turnedOff(s: WorkflowSettings, view: RunView, key: Skippable): boolean {
  return s.stages[key].use === 'off' || view.task?.skipped.includes(key) === true;
}

/** The six stages of the pipeline at the task's current position. Null when there is no task. */
export function stageChips(view: RunView, s: WorkflowSettings): StageChip[] | null {
  const task = view.task;
  if (!task) return null;
  const plan = view.plan;
  const changes = view.changes.filter(live);
  const built = changes.some((c) => c.tip !== null);
  const reviewed = changes.length > 0 && changes.every((c) => c.approved || c.state !== 'open');
  const handed = changes.length > 0 && changes.every((c) => c.state !== 'open');
  const landed = changes.length > 0 && changes.every((c) => TERMINAL.includes(c.state) || c.state === 'handed_off');

  const chips: StageChip[] = [];

  const planOff = turnedOff(s, view, 'plan');
  chips.push({ id: 'plan', label: 'Plan', state: planOff ? 'off' : plan?.id ? 'done' : 'current' });

  if (turnedOff(s, view, 'planReview')) {
    chips.push({ id: 'planReview', label: 'Plan review', state: 'off' });
  } else {
    const approved = plan?.approved === true;
    const signed = s.planSignOff !== 'me' || plan?.signedOff === true;
    const state: StageState = approved && signed ? 'done' : plan?.id || planOff ? 'current' : 'pending';
    const detail =
      approved && !signed
        ? 'needs your sign-off'
        : plan && plan.cap > 0 && state === 'current'
          ? `round ${Math.min(plan.rounds + 1, plan.cap)} of ${plan.cap}`
          : undefined;
    chips.push({ id: 'planReview', label: 'Plan review', state, ...(detail ? { detail } : {}) });
  }

  const planReady = plan !== null && plan.missing === null;
  chips.push({ id: 'build', label: 'Build', state: built ? 'done' : planReady ? 'current' : 'pending' });

  if (turnedOff(s, view, 'review')) {
    chips.push({ id: 'review', label: 'Review', state: 'off' });
  } else {
    const waiting = changes.filter((c) => c.state === 'open' && c.tip !== null && !c.approved);
    const worst = waiting.reduce<RunChange | null>((a, c) => (a === null || c.reviewRounds > a.reviewRounds ? c : a), null);
    const state: StageState = built && reviewed ? 'done' : built ? 'current' : 'pending';
    const detail = state === 'current' && worst && worst.reviewCap > 0 ? `round ${Math.min(worst.reviewRounds + 1, worst.reviewCap)} of ${worst.reviewCap}` : undefined;
    chips.push({ id: 'review', label: 'Review', state, ...(detail ? { detail } : {}) });
  }

  if (turnedOff(s, view, 'cleanup')) {
    chips.push({ id: 'cleanup', label: 'Clean-up', state: 'off' });
  } else {
    chips.push({ id: 'cleanup', label: 'Clean-up', state: handed ? 'done' : built && reviewed ? 'current' : 'pending' });
  }

  const landing = changes.some((c) => c.state !== 'open' && !TERMINAL.includes(c.state));
  chips.push({ id: 'land', label: 'Land', state: landed ? 'done' : handed || landing ? 'current' : 'pending' });
  return chips;
}

/** One reason the user's attention is wanted. */
export interface NeedsYou {
  id: string;
  text: string;
}

/** The `repo branch` name of a change, as the panel shows it. */
export const changeName = (c: RunChange): string => `${c.repo.split('/').pop() ?? c.repo} ${c.branch}`;

/**
 * The "Needs you" items for the active task. `allowRequests` counts the
 * pending Allow cards of workflow kinds; the cards themselves sit in the consent slot.
 */
export function needsYou(view: RunView, s: WorkflowSettings, allowRequests: number): NeedsYou[] {
  const out: NeedsYou[] = [];
  const plan = view.plan;
  // The same test as the Sign off plan button: with plan review off (or skipped) the plan waits
  // only for the user, approved or not.
  if (signOffPlan(view, s)) out.push({ id: 'signoff', text: 'plan sign-off' });
  if (view.task && plan?.id && plan.cap > 0 && plan.rounds >= plan.cap && !plan.approved) {
    out.push({ id: 'cap-planReview', text: 'plan review cap' });
  }
  for (const c of view.changes) {
    const name = changeName(c);
    if (c.state === 'open' && c.reviewCap > 0 && c.reviewRounds >= c.reviewCap && !c.approved) out.push({ id: `cap-review:${name}`, text: `review cap · ${name}` });
    if (c.state === 'open' && c.smoketestCap > 0 && c.smoketestRounds >= c.smoketestCap && c.missing.some((m) => /smoketest/i.test(m))) {
      out.push({ id: `cap-smoketest:${name}`, text: `smoketest cap · ${name}` });
    }
    if (c.state === 'land_failed') out.push({ id: `failed:${name}`, text: `land failed · ${name}` });
    if (c.state === 'awaiting_merge') out.push({ id: `merge:${name}`, text: `merge · ${name}` });
    if (c.state === 'handed_off') out.push({ id: `ready:${name}`, text: `ready for review · ${name}` });
  }
  if (allowRequests > 0) out.push({ id: 'allow', text: allowRequests === 1 ? 'allow request' : `${allowRequests} allow requests` });
  return out;
}

/** The consent kinds the workflow registers (host/crates/workflow kinds.rs). */
export const WORKFLOW_CONSENT_KINDS = ['land', 'ready', 'drop', 'skip', 'allowMore', 'reopen'];

export const isWorkflowConsent = (kind: string): boolean => WORKFLOW_CONSENT_KINDS.includes(kind);

/** `#N` for a PR URL ending `/pull/N`, else null. */
export function prNumber(pr: string | null): string | null {
  const m = pr ? /\/pull\/(\d{1,9})\/?$/.exec(pr) : null;
  return m ? `#${m[1]}` : null;
}

/** A button on one change: the host op `workflow_change_action`. */
export interface ChangeControl {
  action: 'merge' | 'decline' | 'drop' | 'retry' | 'ready';
  label: string;
  primary?: boolean;
  danger?: boolean;
}

/** The buttons a change shows for what awaits the user. The host re-checks each. */
export function changeControls(c: RunChange): ChangeControl[] {
  switch (c.state) {
    case 'awaiting_merge':
      return [
        { action: 'merge', label: 'Merge', primary: true },
        { action: 'decline', label: 'Decline' },
        { action: 'drop', label: 'Drop change', danger: true },
      ];
    case 'land_failed':
      return [
        { action: 'retry', label: 'Retry land', primary: true },
        { action: 'drop', label: 'Drop change', danger: true },
      ];
    case 'handed_off':
      return [{ action: 'ready', label: 'Ready for review', primary: true }];
    default:
      return [];
  }
}

/** The plan id to sign off, when the user's Sign off plan is what the task waits for. */
export function signOffPlan(view: RunView, s: WorkflowSettings): string | null {
  const plan = view.plan;
  if (!view.task || view.task.status !== 'active' || !plan?.id || s.planSignOff !== 'me' || plan.signedOff) return null;
  const reviewNeeded = s.stages.planReview.use === 'required' || (s.stages.planReview.use === 'auto' && !view.task.skipped.includes('planReview'));
  return plan.approved || !reviewNeeded ? plan.id : null;
}

/** One entry of the task's ⋯ menu; `fields` ride the host op `workflow_task_action`. */
export interface TaskMenuEntry {
  id: string;
  label: string;
  action?: 'skip' | 'allowMore' | 'park' | 'reopen' | 'visual' | 'notVisual' | 'end';
  fields?: { stage?: string; n?: number };
  /** A Drop change entry: the change it drops. */
  change?: RunChange;
  danger?: boolean;
  submenu?: TaskMenuEntry[];
}

const SKIPPABLE: { id: Skippable | 'smoketest'; label: string }[] = [
  { id: 'plan', label: 'Plan' },
  { id: 'planReview', label: 'Plan review' },
  { id: 'review', label: 'Review' },
  { id: 'cleanup', label: 'Clean-up' },
  { id: 'smoketest', label: 'Smoketest' },
];

const ROUNDS: { id: 'planReview' | 'review' | 'smoketest'; label: string }[] = [
  { id: 'planReview', label: 'plan review' },
  { id: 'review', label: 'review' },
  { id: 'smoketest', label: 'smoketest' },
];

/** The task row's ⋯ menu: what the user may do to the task in its current status. */
export function taskMenu(view: RunView, s: WorkflowSettings): TaskMenuEntry[] {
  const task = view.task;
  if (!task) return [];
  const out: TaskMenuEntry[] = [];
  if (task.status === 'active') {
    const skips = SKIPPABLE.filter((st) => s.stages[st.id].use !== 'off' && !task.skipped.includes(st.id)).map((st) => ({
      id: `skip:${st.id}`,
      label: st.label,
      action: 'skip' as const,
      fields: { stage: st.id },
    }));
    if (skips.length) out.push({ id: 'skip', label: 'Skip a stage', submenu: skips });
    const more = ROUNDS.filter((st) => s.stages[st.id].use !== 'off').map((st) => ({
      id: `allowMore:${st.id}`,
      label: `Allow 2 more ${st.label} rounds`,
      action: 'allowMore' as const,
      fields: { stage: st.id, n: 2 },
    }));
    if (more.length) out.push({ id: 'allowMore', label: 'Allow more rounds', submenu: more });
    out.push({ id: 'park', label: 'Park', action: 'park' });
  }
  if (task.status === 'parked' || task.status === 'handed_off') out.push({ id: 'reopen', label: 'Reopen', action: 'reopen' });
  if (task.status === 'active' || task.status === 'parked') {
    out.push(task.visual ? { id: 'notVisual', label: 'Not visual', action: 'notVisual' } : { id: 'visual', label: 'Mark visual', action: 'visual' });
  }
  const droppable = view.changes.filter((c) => ['open', 'handed_off', 'awaiting_merge', 'land_failed'].includes(c.state));
  if (droppable.length) {
    out.push({
      id: 'drop',
      label: 'Drop change',
      danger: true,
      submenu: droppable.map((c) => ({ id: `drop:${c.target.repo} ${c.target.branch}`, label: changeName(c), change: c, danger: true })),
    });
  }
  out.push({ id: 'end', label: 'End task', action: 'end', danger: true });
  return out;
}
