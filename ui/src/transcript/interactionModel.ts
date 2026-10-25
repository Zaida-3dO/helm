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

const RECOMMENDED_TAG = /\s*[([](?:recommended|default)[)\]]\s*$|\s+[-–—]\s*(?:recommended|default)\s*$/i;

/** An option written "Use SQLite (Recommended)": the label without the tag, and whether it was there. */
export function splitRecommended(option: string): { label: string; recommended: boolean } {
  const label = option.replace(RECOMMENDED_TAG, '').trim();
  return { label: label || option, recommended: label !== option.trim() };
}

/**
 * The option a question popup puts focus on, so Enter takes the default: the one tagged
 * recommended, else the first. Only questions have one; an approval or a plan never does, so a
 * stray Enter cannot allow anything.
 */
export function defaultIndex(kind: string, options: readonly string[]): number | null {
  if (kind !== 'question' || options.length === 0) return null;
  const tagged = options.findIndex((o) => splitRecommended(o).recommended);
  return tagged >= 0 ? tagged : 0;
}
