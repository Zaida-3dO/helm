// The selected-text toolbar's pure parts: which rows a selection covers and how a
// quote enters the draft. The toolbar component reads the DOM; this file does not.

export const MAX_SELECTION_CHARS = 16_000;
/** Clicks right after the selection changed are ignored (a drag ending over a button). */
export const CLICK_GUARD_MS = 300;

/** The excerpt as a block quote for the draft, followed by a blank line to write in. */
export function quoteForDraft(text: string): string {
  return `${text
    .split('\n')
    .map((l) => (l ? `> ${l}` : '>'))
    .join('\n')}\n\n`;
}
