// Permission modes for the composer's mode picker: the chip picks the mode for the next turn.

const LABELS: Record<string, string> = {
  bypassPermissions: 'Bypass permissions',
  default: 'Asks permission',
  acceptEdits: 'Accept edits',
  dontAsk: "Don't ask",
  auto: 'Auto mode',
  plan: 'Plan mode',
};

export function permissionLabel(mode: string | null): string {
  if (!mode) return 'Bypass permissions';
  return LABELS[mode] ?? mode;
}

/** The modes the picker offers, in order, with what each means. */
export const PICKABLE_MODES: { value: string; title: string; detail: string }[] = [
  { value: 'bypassPermissions', title: 'Bypass permissions', detail: 'Runs every tool without asking' },
  { value: 'default', title: 'Asks permission', detail: 'Asks before it edits or runs anything' },
  { value: 'acceptEdits', title: 'Accept edits', detail: 'Edits files freely; asks before commands' },
  { value: 'plan', title: 'Plan mode', detail: 'Reads and plans; changes nothing' },
];

/** Picks made in this window, by chat, until the live mode catches up (survives a remount). */
const picks = new Map<string, string>();

export function pickedMode(chatId: string): string | null {
  return picks.get(chatId) ?? null;
}

export function setPickedMode(chatId: string, mode: string | null): void {
  if (mode) picks.set(chatId, mode);
  else picks.delete(chatId);
}

/**
 * What the chip shows: a pick the CLI has not confirmed yet wins over the live mode; with neither,
 * bypass (Skip permission prompts is on by default). `pending` is true while the pick waits.
 */
export function shownMode(picked: string | null, live: string | null): { mode: string; pending: boolean } {
  if (picked && picked !== live) return { mode: picked, pending: true };
  return { mode: live ?? 'bypassPermissions', pending: false };
}

/** The chip's tooltip. */
export function permissionTooltip(mode: string, pending: boolean): string {
  const what = PICKABLE_MODES.find((m) => m.value === mode)?.detail ?? permissionLabel(mode);
  return pending ? `${permissionLabel(mode)}: ${what}. Applies from the next turn.` : `${permissionLabel(mode)}: ${what}. Click to change it for this chat.`;
}
