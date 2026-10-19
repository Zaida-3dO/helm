// The interaction card's pure rules, tested.

import type { InteractionAnswer } from '../api/ops.ts';

/** Keys in the first 400 ms after a card (or a new step) appears are ignored. */
export const KEY_GUARD_MS = 400;

/** The eyebrow over the title. */
export function eyebrowOf(kind: string): string {
  if (kind === 'question') return 'Question';
  if (kind === 'plan') return 'Plan';
  return 'Approval';
}

/** 1–9 pick the matching option; anything else (or a number past the options) picks nothing. */
export function pickIndex(key: string, count: number): number | null {
  if (!/^[1-9]$/.test(key)) return null;
  const n = Number(key) - 1;
  return n < count ? n : null;
}

/** What choosing an option sends: a plan's approval is a confirmation; anything else its value. */
export function optionAnswer(kind: string, option: string): InteractionAnswer {
  if (kind === 'plan') return { value: option, confirmed: option === 'Approve plan' };
  return { value: option };
}
