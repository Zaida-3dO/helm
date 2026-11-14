// The gatekeeper's screens as pure functions over the host's audit rows and settings view, so
// the wording and the rules are one tested table. Plain language throughout: nothing here names
// a model tier or an internal state back to the reader beyond what they can act on.

import type { GateAsk, GateSettingsView } from './types.ts';

const MIN = 60_000;

/** "just now", "N min ago", "N h ago", "N d ago" — coarse, as a review list reads. */
export function agoText(now: number, then: number): string {
  const d = Math.max(0, now - then);
  if (d < MIN) return 'just now';
  if (d < 60 * MIN) return `${Math.floor(d / MIN)} min ago`;
  if (d < 24 * 60 * MIN) return `${Math.floor(d / (60 * MIN))} h ago`;
  return `${Math.floor(d / (24 * 60 * MIN))} d ago`;
}

/** "2.3 s" or "450 ms" for the History detail's timing. */
export function tookText(tookMs: number | null | undefined): string | null {
  if (tookMs === null || tookMs === undefined || tookMs < 0) return null;
  return tookMs < 1000 ? `${Math.round(tookMs)} ms` : `${(tookMs / 1000).toFixed(1)} s`;
}

/** What the card shows while the gate holds it, or after a failed hold. Null for a
 *  decided or escalated card, where something else replaces the line. */
export function gateLine(state: string, note?: string): string | null {
  if (state === 'checking') return 'Checking whether this needs you…';
  if (state === 'failed') return note?.trim() || "Helm couldn't check this one.";
  return null;
}

/** The review chip's word for one verdict, as History and the review list say it. */
export function verdictLabel(verdict: string): string {
  switch (verdict) {
    case 'checking':
      return 'Checking';
    case 'decided':
      return 'Decided for you';
    case 'escalated':
      return 'Asked you';
    case 'failed':
      return "Couldn't check";
    case 'passed':
      return 'Left for you';
    default:
      return verdict;
  }
}

/** The word for a record's lifecycle state, for the History detail. Null for states
 *  that need no extra line (open and answered read from the other facts). */
export function stateLabel(state: string): string | null {
  switch (state) {
    case 'parked':
      return 'Waiting for your answer';
    case 'withdrawn':
      return 'Withdrawn';
    case 'kept':
      return 'You left it';
    case 'vetoed':
      return 'You changed the answer';
    case 'expired':
      return 'Expired';
    default:
      return null;
  }
}

/** One channel by its plain name, for History rows. */
export function channelLabel(channel: string): string {
  switch (channel) {
    case 'question':
      return 'Question';
    case 'plan':
      return 'Plan';
    case 'flag':
      return 'Flag';
    case 'telegram_ask':
      return 'Telegram ask';
    case 'board_pick':
      return 'Board pick';
    default:
      return 'Question';
  }
}

/** The mark the user left on a record, or null when it is unmarked. */
export function reviewLabel(review: string): string | null {
  switch (review) {
    case 'fine':
      return 'Fine';
    case 'should_have_asked':
      return 'Should have asked me';
    case 'didnt_need_me':
      return "Didn't need me";
    default:
      return null;
  }
}

/** A row can be marked only while it waits for that mark: "should have asked me" on a decision
 *  still standing, "didn't need me" on an ask you already answered. */
export function canReview(ask: GateAsk): boolean {
  if (ask.review !== 'pending') return false;
  if (ask.verdict === 'decided') return ask.state === 'open' || ask.state === 'kept';
  if (ask.verdict === 'escalated') return ask.state === 'answered';
  return false;
}

/** "Change answer" stands only on a row Helm answered for the user, before it settles. */
export function canChangeAnswer(ask: GateAsk): boolean {
  return ask.verdict === 'decided' && (ask.state === 'open' || ask.state === 'kept') && ask.review === 'pending';
}

/** The one line of a review row: what was chosen, for what, and why. */
export function reviewLine(ask: GateAsk): string {
  const choice = ask.choice ? `«${ask.choice}»` : 'an answer';
  const reason = ask.reason?.trim();
  return `Helm chose ${choice} for “${ask.question}”${reason ? ` because ${reason}` : ''}.`;
}

/** The frequency pick's word. */
export function frequencyLabel(v: string): string {
  return v === 'less' ? 'Less' : v === 'more' ? 'More' : 'Balanced';
}

/** What the chosen frequency means, in one sentence under the picker. */
export function barNote(s: Pick<GateSettingsView, 'askFrequency' | 'bar'>): string {
  const percent = Math.round(s.bar * 100);
  if (s.askFrequency === 'less') return `Helm answers safe questions whenever it is at least ${percent}% sure, and comes to you rarely.`;
  if (s.askFrequency === 'more') return `Helm answers only questions it is at least ${percent}% sure about, and comes to you more often.`;
  return `Helm answers safe questions when it is at least ${percent}% sure.`;
}

/** The History detail's fact rows: label and value, missing facts left out. */
export function detailRows(ask: GateAsk): Array<{ label: string; value: string }> {
  const out: Array<{ label: string; value: string }> = [];
  out.push({ label: 'Asked', value: new Date(ask.askedAt).toLocaleString() });
  const took = tookText(ask.tookMs);
  if (took) out.push({ label: 'Judged in', value: took });
  if (ask.model) out.push({ label: 'Judged by', value: ask.model });
  if (ask.confidence !== undefined) out.push({ label: 'Confidence', value: `${Math.round(ask.confidence * 100)}%` });
  if (ask.risk) out.push({ label: 'Risk', value: ask.risk === 'low' ? 'Low' : ask.risk === 'high' ? 'High' : 'Medium' });
  if (ask.reason) out.push({ label: 'Because', value: ask.reason });
  if (ask.choice) out.push({ label: 'Chose', value: `«${ask.choice}»` });
  const state = stateLabel(ask.state);
  if (state) out.push({ label: 'Status', value: state });
  if (ask.answer) out.push({ label: 'Answer', value: ask.answer });
  if (ask.answeredVia) out.push({ label: 'Answered', value: `From ${ask.answeredVia === 'helm' ? 'Helm' : ask.answeredVia === 'telegram' ? 'Telegram' : ask.answeredVia === 'board' ? 'the task board' : 'the chat'}` });
  const review = reviewLabel(ask.review);
  if (review) out.push({ label: 'Your mark', value: review + (ask.reviewNote ? ` — ${ask.reviewNote}` : '') });
  return out;
}
