// Pure helpers for the top bar, kept out of the .tsx so node:test can import them.

/** The project label until projects load: the chat folder's last segment. */
export function folderLabel(cwd: string | null | undefined): string {
  if (!cwd) return '';
  const parts = cwd.split(/[\\/]+/).filter(Boolean);
  return parts.at(-1) ?? cwd;
}

/** Whether a press should start a window drag: left button, no Ctrl, not on a control. */
export function startsDrag(target: { closest(sel: string): unknown } | null, button: number, ctrl: boolean): boolean {
  if (button !== 0 || ctrl || !target) return false;
  return !target.closest('button, input, select, textarea, a, [role="tab"], [role="button"], [contenteditable="true"], .no-drag');
}
