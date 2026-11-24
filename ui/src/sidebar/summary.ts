// The hover card's summary rules (the host side lives in the chats crate; the card asks here).
//
// - The card opens on the ordinary hover delay showing the last reply. A second later
//   ([`SUMMARY_DELAY_MS`]) it wants the two-sentence summary instead — what the chat is about
//   and where it stands. A summary already riding the row shows at once; otherwise the card
//   asks `chat_summarize` once and, while the host says it is generating, asks again every
//   [`SUMMARY_POLL_MS`] for at most [`SUMMARY_POLLS`] times. When nothing has arrived by then
//   the card simply keeps showing the last reply: no spinner, no error, no second round until
//   the next hover.
// - A summary is fresh while it was made at or after the chat's last activity — the same rule
//   the host applies, so a stale one is never shown.

import type { SidebarRow } from '../api/projects.ts';

/** After the card opens: how long a hover must last before it asks for the summary. */
export const SUMMARY_DELAY_MS = 1_000;
/** While the host is generating: how often the card asks again, for at most `SUMMARY_POLLS`. */
export const SUMMARY_POLL_MS = 1_500;
export const SUMMARY_POLLS = 6;

/** What `chat_summarize` answers. */
export interface SummaryAnswer {
  summary: string | null;
  summaryAt: number | null;
  generating: boolean;
}

/** The summary to show for a row, or null when there is none or it predates the chat's last
 *  activity. Same rule as the host's: made at or after the last activity means it covers it. */
export function freshSummary(
  r: {
    summary?: string | null;
    summaryAt?: number | null;
    lastActivityAt: number;
    updatedAt: number;
  },
): string | null {
  const text = r.summary?.trim();
  if (!text || r.summaryAt == null) return null;
  const last = r.lastActivityAt || r.updatedAt;
  return r.summaryAt >= last ? text : null;
}

/** Whether a `chat_summarize` answer is a summary the card may show for this row. */
export function freshAnswer(
  a: SummaryAnswer,
  r: Pick<SidebarRow, 'lastActivityAt' | 'updatedAt'>,
): string | null {
  const text = a.summary?.trim();
  if (!text || a.summaryAt == null) return null;
  return a.summaryAt >= (r.lastActivityAt || r.updatedAt) ? text : null;
}
