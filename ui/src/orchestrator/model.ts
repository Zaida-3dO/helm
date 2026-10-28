// Pure rules for the Orchestrator switch and the settings page.
// No React here, so node:test can run them.

import type { ChatMode, ChatModeView, LandMode, MergeMethod, RepoRule } from './api.ts';

export type SwitchLook = 'off' | 'auto' | 'on-user' | 'on-agent' | 'unavailable';

export interface SwitchModel {
  look: SwitchLook;
  /** The tooltip, word for word. */
  tooltip: string;
  /** False for the inert states (the turn is running, or the switch is unavailable). */
  clickable: boolean;
  /** What a click asks the host for; null when inert. */
  next: ChatMode | null;
  /** The chat's turn is running (dimmed). */
  dimmed: boolean;
}

/**
 * How the composer switch looks for one chat. `busy` is the chat's own status from the composer
 * (the host's `running` flag only changes when the switch is re-read, the chat status is live).
 */
export function switchModel(view: ChatModeView, busy: boolean): SwitchModel {
  if (view.unavailable) {
    return { look: 'unavailable', tooltip: view.unavailable, clickable: false, next: null, dimmed: true };
  }
  const running = busy || view.running;
  if (view.mode === 'on') {
    const byAgent = view.setBy === 'agent';
    const base = byAgent
      ? `Orchestrator: on. Turned on by the agent${view.reason ? `: ${view.reason}` : ''}.`
      : 'Orchestrator: on for this chat.';
    return {
      look: byAgent ? 'on-agent' : 'on-user',
      tooltip: running ? `${base} You can turn it off when this turn ends.` : `${base} Click to turn it off.`,
      clickable: !running,
      next: running ? null : 'off',
      dimmed: running,
    };
  }
  const tail = 'Click to turn it on now; it applies from the next turn.';
  return view.mode === 'auto'
    ? { look: 'auto', tooltip: `Orchestrator: off. The agent may turn it on for a task. ${tail}`, clickable: true, next: 'on', dimmed: false }
    : { look: 'off', tooltip: 'Orchestrator: off for this chat. Click to turn it on; it applies from the next turn.', clickable: true, next: 'on', dimmed: false };
}

/** Comma- or newline-separated names to a list: trimmed, empties dropped, repeats dropped. */
export function listFromText(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[\n,]/)) {
    const p = part.trim();
    if (p && !out.includes(p)) out.push(p);
  }
  return out;
}

export function listToText(list: string[]): string {
  return list.join(', ');
}

const MODES: LandMode[] = ['draft', 'merge'];
/** A rule may also fast-forward (the base moves to the head; GitHub writes no commit). */
const METHODS: MergeMethod[] = ['merge', 'squash', 'rebase', 'fast_forward'];

/** One repository rule per line: `host/owner/name draft|merge [merge|squash|rebase|fast_forward]`. */
export function rulesFromText(text: string): { rules: RepoRule[] } | { error: string } {
  const rules: RepoRule[] = [];
  for (const raw of text.split('\n')) {
    const line = raw.trim();
    if (!line) continue;
    const [repo, mode, method, ...extra] = line.split(/\s+/);
    if (!repo || !mode || !MODES.includes(mode as LandMode)) {
      return { error: `"${line}": write it as host/owner/name draft or host/owner/name merge [method].` };
    }
    if (extra.length > 0 || (method !== undefined && !METHODS.includes(method as MergeMethod))) {
      return { error: `"${line}": the method is merge, squash, rebase or fast_forward.` };
    }
    if (mode === 'draft' && method) return { error: `"${line}": a draft rule takes no method.` };
    rules.push(method ? { repo, mode: mode as LandMode, method: method as MergeMethod } : { repo, mode: mode as LandMode });
  }
  return { rules };
}

export function rulesToText(rules: RepoRule[]): string {
  return rules.map((r) => [r.repo, r.mode, r.method].filter(Boolean).join(' ')).join('\n');
}
