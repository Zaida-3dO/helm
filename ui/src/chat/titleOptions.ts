// Pure rules of the rename suggestions (Rename chat in the sidebar and the top bar), kept out of the
// .tsx so node:test can import them.

export const MAX_OPTIONS = 3;

/** The options to offer from the host's `title_suggest` answer: its `titles` (or the single `title`
 * of an older host), trimmed, one line each, none repeated (ignoring case) or equal to what the
 * field held when the box opened, at most three. Anything else gives none. */
export function suggestionOptions(answer: unknown, opened: string): string[] {
  const a = (answer ?? {}) as { titles?: unknown; title?: unknown };
  const raw = Array.isArray(a.titles) ? a.titles : typeof a.title === 'string' ? [a.title] : [];
  const out: string[] = [];
  const seen = new Set([opened.trim().toLowerCase()]);
  for (const t of raw) {
    if (typeof t !== 'string') continue;
    const s = t.trim();
    if (!s || /[\u0000-\u001f\u007f]/.test(s) || seen.has(s.toLowerCase())) continue;
    seen.add(s.toLowerCase());
    out.push(s);
    if (out.length >= MAX_OPTIONS) break;
  }
  return out;
}

/** Suggestions are offered only while the field still holds what it held when the box opened: once
 * the person has typed, their text is theirs and a suggestion never replaces it. */
export function suggestionsVisible(value: string, opened: string): boolean {
  return value === opened;
}

/** How long the rename box waits for the host before it stops and says so. The host gives up on
 * the model after 25 seconds and answers by then; this is the box's own limit for a host that
 * never does. */
export const SUGGEST_WAIT_MS = 35_000;

/** What the box shows once the host has answered or failed: the options, and a plain line when
 * the person should know something (the options are not the model's, or there are none). */
export interface SuggestionResult {
  options: string[];
  note: string;
}

/** Ask the host for names and always come back: with the host's answer, with the reason it could
 * not answer, or with a timeout note once `waitMs` has passed. Never waits on nothing. */
export async function askForNames(
  ask: () => Promise<unknown>,
  opened: string,
  waitMs: number = SUGGEST_WAIT_MS,
): Promise<SuggestionResult> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timedOut = new Promise<SuggestionResult>((resolve) => {
    timer = setTimeout(() => resolve({ options: [], note: 'No names came back in time. Type one in.' }), waitMs);
  });
  const answered = ask().then(
    (answer): SuggestionResult => {
      const options = suggestionOptions(answer, opened);
      const said = (answer as { message?: unknown } | null)?.message;
      const note = typeof said === 'string' ? said.trim() : '';
      return { options, note: options.length || note ? note : 'No names came back. Type one in.' };
    },
    (e: unknown): SuggestionResult => {
      const why = e instanceof Error ? e.message.trim() : '';
      return { options: [], note: why || 'Could not ask for names. Type one in.' };
    },
  );
  try {
    return await Promise.race([answered, timedOut]);
  } finally {
    clearTimeout(timer);
  }
}
