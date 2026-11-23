// The in-flight list's rules. Pure: given the pending submissions and the thread, says how
// each one is shown and which actions it offers. Nothing here waits for a turn to end: a
// message is in flight until Claude has read it or it has been put back to the composer.

import type { Capabilities, ChatState, MessageRow, Submission } from '../api/types.ts';
import { untilText } from '../queue/model.ts';
import { toolArgs, toolIdentity, toolLabel, toolStatus } from '../transcript/toolKinds.ts';

export type InFlightState = 'pending' | 'sending' | 'uncertain' | 'failed';

export interface InFlightView {
  submission: Submission;
  /** The stable data-state hook value and the label's basis. */
  state: InFlightState;
  label: string;
  /** Why an unread message waits, in words ("Waiting for Bash to finish"); null for any other state. */
  reason: string | null;
  tone: InFlightTone;
  actions: InFlightAction[];
}

export type InFlightTone = 'muted' | 'amber' | 'error';

export type InFlightAction = 'withdraw' | 'recover' | 'startAnyway';

export interface InFlightContext {
  capabilities: Capabilities;
  /** What an unread message is waiting behind (`waitReason`, or a queue hold's line from
   * `holdLine`); defaults to the plain sentence. */
  reason?: string;
  /** The chat's start is held by Claude's plan windows (`claudeHold`): each waiting message
   * offers Start anyway. */
  claudeHold?: boolean;
  /** The rows the transcript holds. A message whose own row is among them is not listed again. */
  rows?: readonly MessageRow[];
}

const NEXT_STEP = 'Claude reads it at its next step';

/** The live state of a start the Claude plan windows hold: the one case Start anyway answers. */
export function claudeHold(state: ChatState | null | undefined): boolean {
  return state?.kind === 'queued' && state.reason === 'claude_capacity';
}

/** The queued state's own waiting line, in plain English: a Claude hold names the allowance,
 *  how full it reads and when it opens; the app's chat limit says what it is. Any other wait
 *  (and a chat that is not queued) answers null and keeps the generic line. */
export function holdLine(state: ChatState | null | undefined, now: number): string | null {
  if (state?.kind !== 'queued') return null;
  if (state.reason === 'claude_capacity') {
    const used = state.pct != null ? ` (${Math.round(state.pct)}% used)` : '';
    const opens = state.until != null ? ` — opens again ${untilText(state.until, now)}` : '';
    return `Waiting for Claude's plan allowance${used}${opens}`;
  }
  if (state.reason === 'at_limit') return 'Waiting for a chat place — Helm is running all its chats at once';
  return null;
}

/**
 * Why a message Claude holds unread has not been read yet. Claude reads a message between steps,
 * so while a tool call is still running the message waits for it; otherwise it waits for the step
 * that is being written to end. Read from the live tool rows, newest first.
 */
export function waitReason(rows: readonly MessageRow[]): string {
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i]!;
    if (r.role === 'toolResult' && toolStatus(r) === 'running') return `Waiting for ${waitingFor(r)} to finish`;
    // The turn's own start: tools before it are history.
    if (r.role === 'user' && r.details?.mode !== 'steer') break;
  }
  return NEXT_STEP;
}

/** The running call, in words a user knows: Helm's own tools are "a Helm tool", and a call made
 *  through `app_mcp_call` is named by the MCP tool it reaches. */
function waitingFor(r: MessageRow): string {
  const { source, server, tool } = toolIdentity(r);
  if (source === 'host' && tool === 'app_mcp_call') {
    const target = toolArgs(r).name;
    if (typeof target === 'string' && target && !target.startsWith('app_')) return toolLabel(`mcp__${target}`);
  }
  if (source === 'host') return 'a Helm tool';
  return source === 'mcp' ? `${server} · ${tool}` : toolLabel(r.toolName);
}

/** In flight in the order they were sent. The host's list already excludes settled rows. */
export function orderInFlight(pending: Submission[]): Submission[] {
  return pending.filter((s) => s.state !== 'withdrawn' && s.state !== 'superseded' && s.state !== 'delivered' && s.state !== 'complete').sort((a, b) => a.createdAt - b.createdAt);
}

export function itemState(s: Submission): InFlightState {
  if (s.state === 'uncertain') return 'uncertain';
  if (s.state === 'rejected') return 'failed';
  if (s.state === 'dispatching') return 'sending';
  // Saved or accepted: sent to Claude, not read by it yet.
  return 'pending';
}

const LABELS: Record<InFlightState, string> = {
  pending: 'Pending',
  sending: 'Sending',
  uncertain: 'Not sent',
  failed: 'Not delivered',
};

function toneOf(state: InFlightState): InFlightTone {
  if (state === 'pending') return 'muted';
  return state === 'failed' ? 'error' : 'amber';
}

/**
 * The messages on their way whose own user row is already in the transcript. A prompt that opens a
 * turn gets its row the moment it is handed over (status "saved"), and a steer gets its row when
 * Claude reads it, but the host keeps listing the message until Claude has read it, which for a
 * cold start is seconds later. From the row on, the row is the message: listing it too shows it
 * twice. Only messages still on their way drop out; a message known unsent keeps its bubble, which
 * carries Recover.
 */
function shownAsRow(pending: Submission[], rows: readonly MessageRow[] | undefined): Set<string> {
  const shown = new Set<string>();
  if (!rows?.length) return shown;
  const onTheirWay = new Set(pending.filter((s) => ['pending', 'sending'].includes(itemState(s))).map((s) => s.id));
  if (onTheirWay.size === 0) return shown;
  for (const r of rows) {
    const id = r.role === 'user' ? r.details?.submissionId : undefined;
    if (id && onTheirWay.has(id)) shown.add(id);
  }
  return shown;
}

export function inFlightView(pending: Submission[], ctx: InFlightContext): InFlightView[] {
  const shown = shownAsRow(pending, ctx.rows);
  return orderInFlight(pending).filter((s) => !shown.has(s.id)).map((s) => {
    const state = itemState(s);
    const actions: InFlightAction[] = [];
    // Withdraw only while Claude holds it unread; Recover only once it is known unsent.
    if (state === 'pending' && s.state === 'accepted' && ctx.capabilities.withdraw) actions.push('withdraw');
    if (state === 'uncertain' || state === 'failed') actions.push('recover');
    // A start the plan windows hold: the user may force this one send through.
    if (state === 'pending' && ctx.claudeHold) actions.push('startAnyway');
    return { submission: s, state, label: LABELS[state], reason: state === 'pending' ? (ctx.reason ?? NEXT_STEP) : null, tone: toneOf(state), actions };
  });
}
