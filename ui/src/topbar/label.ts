// Pure helpers for the top bar, kept out of the .tsx so node:test can import them.

/** The project label until projects load: the chat folder's last segment. */
export function folderLabel(cwd: string | null | undefined): string {
  if (!cwd) return '';
  const parts = cwd.split(/[\\/]+/).filter(Boolean);
  return parts.at(-1) ?? cwd;
}
