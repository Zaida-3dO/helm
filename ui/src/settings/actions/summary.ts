// The words the Actions list shows for one button, kept apart from the component so they can be
// tested: where it shows, where it sends, and a one-line preview of its message.

import type { ActionButton } from '../../api/actions.ts';

export const MAX_PREVIEW_CHARS = 120;

/** Where the button shows (its placement; a hidden button is flagged separately). */
export function showsIn(b: Pick<ActionButton, 'placement'>): string {
  return b.placement === 'topbar' ? 'Top bar' : 'More menu';
}

/** Where the button sends: this chat, or a new chat in the bound project. */
export function sendsTo(b: Pick<ActionButton, 'kind' | 'projectId'>, projectName: string | null): string {
  if (b.kind === 'prompt') return 'This chat';
  if (!b.projectId) return 'New chat (no project yet)';
  return `New chat in ${projectName ?? 'an unavailable project'}`;
}

/** The first non-empty line of the message, cut to one short line. */
export function previewLine(text: string): string {
  const line = text.split(/\r?\n/).find((l) => l.trim() !== '')?.trim() ?? '';
  const chars = [...line];
  return chars.length > MAX_PREVIEW_CHARS ? `${chars.slice(0, MAX_PREVIEW_CHARS - 1).join('')}…` : line;
}

/** What to call the button in a spoken label while its label is still empty. */
export function nameOf(b: Pick<ActionButton, 'label'>): string {
  return b.label.trim() === '' ? 'new button' : b.label.trim();
}
