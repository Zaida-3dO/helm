// What the restart-to-update dialog says. Pure, so it is tested without a window.

import type { UpdateInstallOutcome, UpdatePrepare } from '../api/updates.ts';

const TITLE_MAX = 60;

/** A running chat's line, cut so a long title does not widen the dialog. */
export function busyLine(line: string): string {
  return line.length > TITLE_MAX ? `${line.slice(0, TITLE_MAX - 1).trimEnd()}…` : line;
}

/** The lines the dialog lists: at most four chats, then how many more there are. */
export function busyLines(p: Pick<UpdatePrepare, 'busy' | 'busyTotal'>): string[] {
  const shown = p.busy.slice(0, 4).map(busyLine);
  const more = p.busyTotal - shown.length;
  return more > 0 ? [...shown, `and ${more} more`] : shown;
}

/** The sentence under the title: what happens and what is kept. */
export function restartBody(p: Pick<UpdatePrepare, 'version' | 'busyTotal'>): string {
  const stop = p.busyTotal > 0 ? ' Work that is still running will be stopped:' : '';
  return `Helm will save your drafts, close, install version ${p.version} and open again.${stop}`;
}

/** What to tell the user when the restart did not happen (null when it is under way). */
export function installProblem(outcome: UpdateInstallOutcome): string | null {
  switch (outcome) {
    case 'save_failed':
      return 'Helm could not save your drafts in time, so it did not restart. Nothing was changed.';
    case 'busy':
      return 'Helm is already closing.';
    case 'cancelled':
      return 'The restart was cancelled.';
    case 'restarting':
      return null;
  }
}
