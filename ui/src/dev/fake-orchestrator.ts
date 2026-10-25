// Dev fake for the workflow ops (the composer's Orchestrator switch and Settings → Orchestrator in
// a plain browser). `?orch=` in the page URL picks the start: `off` (master switch off), `auto`
// (new chats start in Auto), `on` (default). Dev builds only; fakeHost.ts imports this file.

import type { ChatMode, ChatModeView, OtherTaskView, RunChangeView, RunPlanView, RunTaskView, RunView, WorkflowSettings } from '../api/generated/workflow/index.ts';
import type { FakeOpHandler } from './fakeHost.ts';

function startOf(): string {
  return typeof location === 'undefined' ? 'on' : (new URLSearchParams(location.search).get('orch') ?? 'on');
}

export function defaultSettings(enabled: boolean): WorkflowSettings {
  return {
    enabled,
    stages: {
      plan: { use: 'required', agent: 'planner' },
      planReview: { use: 'required', agent: 'plan-reviewer', maxRounds: 3 },
      build: { agent: 'coder', startAgents: ['coder', 'worker', 'general-purpose'], buildExempt: [] },
      review: { use: 'required', agent: 'reviewer', maxRounds: 3 },
      cleanup: { use: 'required', agent: 'pr-cleanup' },
      smoketest: { use: 'auto', agent: 'smoketester', maxRounds: 3 },
    },
    orchestratorEdits: 'deny',
    planSignOff: 'review',
    homeRepos: [],
    landing: { default: 'draft', method: 'merge', repos: [], draftLabels: [], flipLabel: null, protectedBranches: ['main', 'master'] },
    moveFast: false,
    landingGuard: true,
    dispatchGate: true,
    newChats: 'auto',
  };
}

/** `?orchrun=` picks the task the fake chats show: plan, signoff, review, cap, landing, failed, others, none. */
function runOf(): string {
  return typeof location === 'undefined' ? 'review' : (new URLSearchParams(location.search).get('orchrun') ?? 'review');
}

const REPO = 'github.com/example/app';

export function change(over: Partial<RunChangeView>): RunChangeView {
  return {
    repo: REPO, branch: 'feat/login', pr: null, state: 'open', failedMode: null, tip: 'a'.repeat(40), approved: false,
    reviewRounds: 1, reviewCap: 3, smoketestRounds: 0, smoketestCap: 3, error: null, missing: [], ...over,
  };
}

export function fakeRun(kind: string): RunView {
  const task: RunTaskView = { runId: 'run-1', title: 'Add the login page', status: 'active', attempt: 1, visual: false, itemId: null, skipped: [] };
  const plan = (over: Partial<RunPlanView>): RunPlanView => ({ id: 'p'.repeat(64), approved: true, signedOff: false, rounds: 1, cap: 3, missing: null, ...over });
  switch (kind) {
    case 'none':
      return { task: null, plan: null, changes: [], otherTasks: [], guard: [], boardQueued: 0 };
    case 'plan':
      return { task, plan: plan({ id: null, approved: false, rounds: 0, missing: 'the task has no plan of record yet; adopt one with app_workflow plan' }), changes: [], otherTasks: [], guard: [], boardQueued: 0 };
    case 'signoff':
      return { task, plan: plan({ missing: "the plan waits for the user's sign-off" }), changes: [], otherTasks: [], guard: [], boardQueued: 0 };
    case 'cap':
      return { task, plan: plan({}), changes: [change({ reviewRounds: 3, missing: ['feat/login: no approving review at the latest commit'] })], otherTasks: [], guard: [], boardQueued: 0 };
    case 'landing':
      return { task, plan: plan({ signedOff: true }), changes: [change({ state: 'awaiting_merge', approved: true, pr: `https://${REPO}/pull/12` }), change({ branch: 'feat/api', state: 'landed', approved: true })], otherTasks: [], guard: [], boardQueued: 0 };
    case 'failed':
      return { task, plan: plan({ signedOff: true }), changes: [change({ state: 'land_failed', approved: true, error: 'The base branch moved; the merge was refused.' })], otherTasks: [{ runId: 'run-0', title: 'Older task', status: 'parked', parkReason: 'chat turned off', waiting: [] }], guard: [], boardQueued: 0 };
    case 'others': {
      // The current task is mid-build; an older task has a draft PR waiting on the user.
      const older: OtherTaskView = {
        runId: 'run-0',
        title: 'Older task',
        status: 'handed_off',
        parkReason: null,
        waiting: [change({ repo: REPO, branch: 'feat/older', state: 'awaiting_merge', approved: true, pr: `https://${REPO}/pull/9` })],
      };
      return { task, plan: plan({ signedOff: true }), changes: [change({})], otherTasks: [older], guard: [], boardQueued: 0 };
    }
    default:
      return { task, plan: plan({ signedOff: true }), changes: [change({ missing: ['feat/login: no approving review at the latest commit'] })], otherTasks: [], guard: [], boardQueued: 0 };
  }
}

export function orchestratorFakeOps(): { prefixes: string[]; handler: FakeOpHandler } {
  const start = startOf();
  let settings = defaultSettings(start !== 'off');
  if (start === 'auto') settings = { ...settings, newChats: 'auto' };
  const modes = new Map<string, { mode: ChatMode; setBy: 'user' | 'agent' | 'default'; reason?: string }>();
  const view = (chatId: string, running: boolean): ChatModeView => {
    const m = modes.get(chatId) ?? { mode: settings.newChats === 'always' ? ('on' as const) : settings.newChats === 'never' ? ('off' as const) : ('auto' as const), setBy: 'default' as const };
    return {
      chatId,
      mode: m.mode,
      setBy: m.setBy,
      ...(m.reason ? { reason: m.reason } : {}),
      inEffect: settings.enabled && m.mode === 'on',
      ...(settings.enabled ? {} : { unavailable: 'Turn on the Orchestrator workflow in Settings → Orchestrator first.' }),
      running,
    };
  };
  const handler: FakeOpHandler = (op, args, ctx) => {
    const chatId = String(args.chatId ?? '');
    switch (op) {
      case 'workflow_settings_get':
        return settings;
      case 'workflow_settings_save':
        settings = args.settings as WorkflowSettings;
        ctx.emit({ kind: 'workflow_settings' });
        return settings;
      case 'workflow_chat_mode_get':
        return view(chatId, false);
      case 'workflow_chat_mode_set': {
        const mode = args.mode as ChatMode;
        const before = view(chatId, false);
        if (mode === 'off' && before.running) throw { code: 'conflict', message: 'A Claude Code chat is running a turn; turn Orchestrator on or off once it has finished.' };
        if (before.mode === mode) return { view: before };
        modes.set(chatId, { mode, setBy: 'user' });
        ctx.emit({ kind: 'workflow_chat_mode', chatId });
        return { view: view(chatId, false), row: `Orchestrator ${mode === 'on' ? 'turned on' : mode === 'off' ? 'turned off' : 'set to Auto'}.` };
      }
      case 'workflow_run_get':
        return fakeRun(runOf());
      case 'workflow_dev_agent_on': {
        // Dev only: pretend the chat's agent turned the Orchestrator on in Auto.
        modes.set(chatId, { mode: 'on', setBy: 'agent', reason: String(args.reason ?? 'multi-step change') });
        ctx.emit({ kind: 'workflow_chat_mode', chatId });
        return view(chatId, false);
      }
      default:
        throw { code: 'refused', message: 'This operation is not available here.' };
    }
  };
  return { prefixes: ['workflow_'], handler };
}
