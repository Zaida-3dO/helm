// The quiet facts at the end of a reply's action row: the turn time and the token chip.

import type { MessageRow } from '../api/types.ts';
import type { Item } from './workBlocks.ts';

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

/** A compact count: 950, 1.2k, 12k, 3.4M. */
export function compactCount(n: number): string {
  if (n < 1000) return String(n);
  if (n < 10_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`;
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
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
