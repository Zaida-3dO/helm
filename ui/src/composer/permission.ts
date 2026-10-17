// Permission-mode labels for the chip.

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

