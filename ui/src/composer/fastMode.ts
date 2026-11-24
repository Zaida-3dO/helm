// The composer's fast-mode bolt. Fast mode is Claude Code's launch switch: quicker replies on
// models that support it, at a higher token price. A chat can hold its own pick (the record's
// `fastMode`); without one it follows the new-chat default of the PC that runs it. The value
// reaches the chat's agent when its worker next starts, so a working chat keeps what it started
// with. A chat on a local model has no fast mode at all: the control is not shown there.

import { isLocalModel } from './modelEffort.ts';

/** The bolt belongs to Claude chats only: a local model ignores fast mode, so a local chat
 *  hides the control entirely rather than showing a switch that does nothing. */
export function fastModeVisible(model: string | null | undefined): boolean {
  return !isLocalModel(model);
}

/** What the bolt shows pressed: the chat's own pick when it has one, else the PC's default.
 *  `null` — the default is not known yet — says "hold the bolt back" rather than guess. */
export function fastModeShown(own: boolean | null, defaultOn: boolean | null): boolean | null {
  return own ?? defaultOn;
}

/** The tooltip: what it does, what it costs, when it takes effect, and where the fallback is. */
export function fastModeTitle(on: boolean): string {
  return (
    `Fast mode for this chat (${on ? 'on' : 'off'}): quicker replies on models that support it, at a higher token price. ` +
    'It applies when this chat’s agent next starts. With no pick of its own a chat follows the default in Settings → Runtime.'
  );
}
