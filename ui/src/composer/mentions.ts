// `@` file mentions: typing `@` and a few letters offers the project's files; picking one writes
// its path into the draft as `@path`, which is how the agent is pointed at a file.
//
// Pure helpers: where the mention is, what a pick writes, and the draft after it.

export interface Mention {
  /** Where the `@` sits in the draft. */
  start: number;
  /** The end of the word the caret is in (the whole word is replaced on a pick). */
  end: number;
  /** What was typed after the `@`, up to the caret. */
  query: string;
}

/** The files menu shows at most this many rows. */
export const MENTION_ROWS = 8;

/** The `@word` the caret is in, or null. The `@` must start the draft or follow whitespace (so an address is left alone). */
export function mentionAt(text: string, caret: number): Mention | null {
  if (caret < 0 || caret > text.length) return null;
  const m = /(^|\s)@([^\s@]*)$/.exec(text.slice(0, caret));
  if (!m) return null;
  const query = m[2]!;
  const rest = /^\S*/.exec(text.slice(caret))![0];
  return { start: caret - query.length - 1, end: caret + rest.length, query };
}

export interface MentionFile {
  /** Absolute. */
  path: string;
  /** Relative to the project in file search; the entry name when a path was typed. */
  label: string;
  kind: 'dir' | 'file' | 'link';
}

/** What a pick writes: `@` and the project-relative path, or the full path when one was typed. */
export function mentionText(item: MentionFile, pathMode: boolean): string {
  const shown = (pathMode ? item.path : item.label).replace(/\\/g, '/');
  const path = item.kind === 'dir' && !shown.endsWith('/') ? `${shown}/` : shown;
  return /\s/.test(path) ? `@"${path}"` : `@${path}`;
}

/** The draft with the mention replaced by `insertion`, and where the caret goes. A file ends with a space, a folder does not. */
export function applyMention(text: string, m: Mention, insertion: string, isDir: boolean): { text: string; caret: number } {
  const head = text.slice(0, m.start) + insertion;
  const tail = text.slice(m.end);
  if (isDir) return { text: head + tail, caret: head.length };
  // A space already after the word is used; otherwise one is added so typing carries on.
  const space = tail.startsWith(' ') ? '' : ' ';
  return { text: head + space + tail, caret: head.length + 1 };
}
