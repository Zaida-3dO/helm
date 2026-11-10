// Small helpers shared by the knowledge sections.

export function errorText(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

export function errorCode(e: unknown): string | null {
  const code = (e as { code?: unknown })?.code;
  return typeof code === 'string' ? code : null;
}

/** What a section shows after a change: a short sentence, or the host's reason it failed. */
export interface Notice {
  kind: 'note' | 'error';
  text: string;
}

/** A size in plain words ("2 KB"). */
export function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${Math.round(bytes / 1024)} KB`;
}
