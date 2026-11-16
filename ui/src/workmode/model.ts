// The working mode's wording as pure functions, so the chip's labels, tooltips and counts line
// are one tested table. The mode's own words come from the host (its view); nothing here invents
// new names for the modes.

import type { ModeCounts, WorkMode, WorkModeView } from '../api/generated/gatekeeper/index.ts';
import type { TopBarTier } from '../topbar/fold.ts';

/** What the user calls each mode, in full. */
export function modeLabel(mode: WorkMode): string {
  return mode === 'run' ? 'Run to completion' : 'Check in with me';
}

/** The chip's short word; empty at the bar's narrow and tight tiers, where the glyph stands alone. */
export function chipWord(mode: WorkMode, tier: TopBarTier): string {
  if (tier === 'narrow' || tier === 'tight') return '';
  return mode === 'run' ? 'Run' : 'Check in';
}

/** The chip's tooltip: the mode, and where it was set. */
export function chipTooltip(view: WorkModeView): string {
  if (view.source === 'board' && view.board) return `${modeLabel(view.effective)} — this board's own choice (${view.board.name})`;
  return `${modeLabel(view.effective)} — set in Helm for every chat`;
}

/** "N decided for you · M need you", the popover's line about what is waiting to be read. */
export function countsLine(counts: ModeCounts): string {
  const decided = counts.decided === 1 ? '1 decided for you' : `${counts.decided} decided for you`;
  const needsYou = counts.needsYou === 1 ? '1 needs you' : `${counts.needsYou} need you`;
  return `${decided} · ${needsYou}`;
}

/** The word for a board's choice in the popover's board row; `null` is "same as the app". */
export function boardChoiceLabel(mode: WorkMode | null): string {
  return mode === null ? 'Same as app' : modeLabel(mode);
}

/** The id the popover and Settings use for the three board choices, so `null` is a real pick. */
export const BOARD_CHOICE_IDS = ['same', 'run', 'check_in'] as const;
export type BoardChoiceId = (typeof BOARD_CHOICE_IDS)[number];

export function choiceToMode(id: BoardChoiceId): WorkMode | null {
  return id === 'same' ? null : id;
}

export function modeToChoice(mode: WorkMode | null): BoardChoiceId {
  return mode ?? 'same';
}
