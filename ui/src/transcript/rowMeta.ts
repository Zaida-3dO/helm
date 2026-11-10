// The quiet facts at the end of a reply's action row: the turn time and the token chip.

import type { MessageRow } from '../api/types.ts';
import type { Item } from './workBlocks.ts';
import { toolArgs } from './toolKinds.ts';

type Usage = NonNullable<MessageRow['details']['usage']>;

/** "less than a second", "12s", "2m 14s", "1h 3m". Rounds like "Worked for" so the two agree. */
export function formatTurnTime(ms: number): string {
  if (ms < 1000) return 'less than a second';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Tokens this reply added: everything except input read back from the cache. */
export function newTokens(u: Usage): number {
  return u.inputTokens + u.cacheWriteTokens + u.outputTokens;
}

/** Refusals saying the prompt outgrew the model's window (the server's own wording, matched loosely).
 * The worker decides when a turn was refused for this reason; keep the two lists in step
 * (worker/src/drivers/claude/env.ts). */
const TOO_LONG_PHRASES = [/prompt is too long/i, /context length exceeded/i, /maximum context length/i, /too many tokens/i];

/**
 * True when an interrupted chat's error is a too-large-for-the-window refusal: "Continue" would
 * re-send the same prompt and fail the same way, so the banner offers the roll over instead.
 */
export function isTooLargeRefusal(error: string | null | undefined): boolean {
  return !!error && TOO_LONG_PHRASES.some((re) => re.test(error));
}

/** Context size at which the chat offers a prominent Roll over button. */
export const ROLL_OVER_TOKENS = 250_000;
/** Below this the context size is not worth showing. */
export const CONTEXT_SHOWN_TOKENS = 50_000;

/**
 * How much context the chat holds now: input, cache reads and cache writes of the newest reply
 * that reports usage. Null when no reply does (or the newest rows are not loaded).
 */
export function contextTokens(rows: MessageRow[]): number | null {
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i]!;
    const u = r.role === 'assistant' ? r.details?.usage : undefined;
    if (u) return u.inputTokens + u.cacheReadTokens + u.cacheWriteTokens;
  }
  return null;
}

/** The host's context reading for a chat (the `chat_context` op): the context the chat's newest
 * recorded turn measured, and the model's real window. Null fields when nothing was measured or
 * no server has stated a window. */
export interface ChatContextReading {
  contextTokens: number | null;
  windowTokens: number | null;
  windowPct?: number | null;
  measuredAtMs?: number | null;
}

/**
 * The context size to show: the newest reply's own reading and the host's last measured turn,
 * whichever is larger — a turn whose last call was refused measures in the usage table but leaves
 * no reply row, and a streaming reply measures on the row before the turn's usage entry lands.
 * With the model's window known, the honest percent comes too (over 100 means it no longer fits).
 * Null when nothing has been measured.
 */
export function contextSize(
  rows: MessageRow[],
  host: ChatContextReading | null,
): { tokens: number; window: number | null; pct: number | null } | null {
  const tokens = Math.max(contextTokens(rows) ?? 0, host?.contextTokens ?? 0);
  if (tokens === 0) return null;
  const window = host?.windowTokens ?? null;
  const pct = window !== null ? Math.round((tokens / window) * 10000) / 100 : null;
  return { tokens, window, pct };
}

/** A compact count: 950, 1.2k, 12k, 3.4M. */
export function compactCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
}

/** How long ago, in words: "just now", "5 minutes ago", "3 hours ago", "2 days ago". */
export function agoText(ms: number): string {
  const s = Math.max(0, ms) / 1000;
  const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? '' : 's'} ago`;
  if (s < 45) return 'just now';
  if (s < 90 * 60) return plural(Math.max(1, Math.round(s / 60)), 'minute');
  if (s < 36 * 3600) return plural(Math.round(s / 3600), 'hour');
  return plural(Math.round(s / 86_400), 'day');
}

/** A message's tooltip: "Today at 3:41 PM · 5 minutes ago" (Yesterday, a weekday within the week, then the date). */
export function messageTimeLabel(timestamp: number, now: number): string {
  const d = new Date(timestamp);
  const n = new Date(now);
  const startOfDay = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((startOfDay(n) - startOfDay(d)) / 86_400_000);
  const time = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  let day: string;
  if (days <= 0) day = 'Today';
  else if (days === 1) day = 'Yesterday';
  else if (days < 7) day = d.toLocaleDateString([], { weekday: 'long' });
  else day = d.toLocaleDateString([], { day: 'numeric', month: 'short', year: d.getFullYear() === n.getFullYear() ? undefined : 'numeric' });
  return `${day} at ${time} · ${agoText(now - timestamp)}`;
}

/** The chip text ("12k tokens") and its tooltip, which says what was left out. */
export function tokenChip(u: Usage): { text: string; title: string } {
  const n = newTokens(u);
  return {
    text: `${compactCount(n)} ${n === 1 ? 'token' : 'tokens'}`,
    title:
      `${n.toLocaleString()} new tokens: ${(u.inputTokens + u.cacheWriteTokens).toLocaleString()} input, ` +
      `${u.outputTokens.toLocaleString()} output. Input read from the cache ` +
      `(${u.cacheReadTokens.toLocaleString()}) is not counted.`,
  };
}

/**
 * The turn time shown in a reply's action row: on the last reply of a turn whose work
 * folded, the same span its "Worked for" row shows. A turn with nothing folded shows no time.
 */
export function turnTimes(items: Item[]): Map<number, number> {
  const out = new Map<number, number>();
  let elapsed: number | null = null;
  let reply: number | null = null;
  const close = () => {
    if (elapsed !== null && reply !== null) out.set(reply, elapsed);
    elapsed = null;
    reply = null;
  };
  for (const it of items) {
    if (it.kind === 'agents') continue;
    if (it.kind === 'fold') elapsed = it.elapsedMs;
    else if (it.row.role === 'user' && it.row.details?.mode !== 'steer') close();
    else if (elapsed !== null && it.row.role === 'assistant' && it.row.text.trim()) reply = it.row.id;
  }
  close();
  return out;
}

/**
 * The skills a turn used, on the turn's last reply: `personal:`, `project:` and `helm:` skills
 * alike, as the names the Skill tool was called with, one per line. Only a reply with text carries
 * them, and a steering message does not start a new turn.
 */
export function skillsByReply(rows: readonly MessageRow[]): Map<number, string> {
  const out = new Map<number, string>();
  let used: string[] = [];
  let reply: number | null = null;
  const close = () => {
    if (used.length > 0 && reply !== null) out.set(reply, used.join('\n'));
    used = [];
    reply = null;
  };
  for (const r of rows) {
    if (r.role === 'user' && r.details?.mode !== 'steer') close();
    else if (r.role === 'toolResult' && r.toolName === 'Skill') {
      const name = toolArgs(r).skill;
      if (typeof name === 'string' && name.trim() && !used.includes(name.trim())) used.push(name.trim());
    } else if (r.role === 'assistant' && r.text.trim()) reply = r.id;
  }
  close();
  return out;
}

/** Whether a chat attached by reference can still be opened: any chat the list holds, or every chat while the list has not loaded. */
export function chatRefOpens(chats: readonly { id: string }[], id: string): boolean {
  return chats.length === 0 || chats.some((c) => c.id === id);
}

