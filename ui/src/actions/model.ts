// The pure rules behind the button set: what shows where, and the edits Settings makes. The host
// validates the same limits on save (host/crates/actions); checking here is only so the editor can
// say what is wrong before the round trip.

import type { ActionButton, ActionSet } from '../api/actions.ts';

export const MAX_BUTTONS = 24;
export const MAX_LABEL_CHARS = 40;
export const MAX_TEXT_BYTES = 16 * 1024;

/** The buttons shown in the top bar, in set order. */
export function topbarButtons(set: Pick<ActionSet, 'buttons'>): ActionButton[] {
  return set.buttons.filter((b) => b.placement === 'topbar' && !b.hidden);
}

/** The buttons in the "..." menu, in set order. */
export function overflowButtons(set: Pick<ActionSet, 'buttons'>): ActionButton[] {
  return set.buttons.filter((b) => b.placement === 'overflow' && !b.hidden);
}

/** A sentence naming the first thing wrong with `b`, or null when it is saveable. */
export function problemWith(b: ActionButton): string | null {
  const label = b.label.trim();
  if (label.length === 0) return 'Give the button a label.';
  if ([...label].length > MAX_LABEL_CHARS) return `A label is at most ${MAX_LABEL_CHARS} characters.`;
  if (b.text.trim().length === 0) return 'Give the button some text to send.';
  if (new TextEncoder().encode(b.text).length > MAX_TEXT_BYTES) return 'The text is over 16 KiB.';
  if (b.kind === 'prompt' && b.projectId !== null) return 'Only a new-chat button binds to a project.';
  return null;
}

/** The first problem across the whole set, with the button it belongs to. */
export function firstProblem(buttons: ActionButton[]): { id: string; message: string } | null {
  if (buttons.length > MAX_BUTTONS) return { id: '', message: `At most ${MAX_BUTTONS} buttons.` };
  for (const b of buttons) {
    const message = problemWith(b);
    if (message) return { id: b.id, message };
  }
  return null;
}

export function newButton(id: string): ActionButton {
  return { id, label: '', kind: 'prompt', text: '', projectId: null, placement: 'topbar', hidden: false, seedId: null };
}

export function addButton(buttons: ActionButton[], b: ActionButton): ActionButton[] {
  return buttons.length >= MAX_BUTTONS ? buttons : [...buttons, b];
}

export function updateButton(buttons: ActionButton[], id: string, patch: Partial<ActionButton>): ActionButton[] {
  return buttons.map((b) => {
    if (b.id !== id) return b;
    const next = { ...b, ...patch, id: b.id, seedId: b.seedId };
    // Switching to a plain prompt drops a project binding the host would refuse.
    return next.kind === 'prompt' ? { ...next, projectId: null } : next;
  });
}

export function removeButton(buttons: ActionButton[], id: string): ActionButton[] {
  return buttons.filter((b) => b.id !== id);
}

/** Move a button one place earlier (-1) or later (+1). Out of range changes nothing. */
export function moveButton(buttons: ActionButton[], id: string, delta: -1 | 1): ActionButton[] {
  const i = buttons.findIndex((b) => b.id === id);
  const j = i + delta;
  if (i < 0 || j < 0 || j >= buttons.length) return buttons;
  const out = buttons.slice();
  [out[i], out[j]] = [out[j]!, out[i]!];
  return out;
}
