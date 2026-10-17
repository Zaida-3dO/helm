// Inline option buttons: find the choices an assistant
// reply offers, so each can be answered with one click.
//
// A list counts as choices only when it is the reply's last list, has 2 to 9 sequential markers
// (`1.`, `1)`, `Option 1`, `A)`, `**A.**`, `Option A:`) and either uses the explicit word "Option"
// or the reply signals that a choice is wanted. Numbered steps, a changelog or a plan that merely
// happens to be numbered must not grow buttons, so plain markers need that signal.

export interface ParsedOption {
  /** "1", "2", ... or "A", "B", ... */
  label: string;
  /** The item's first line, cleaned of markdown, cut at 80 characters: the button's tooltip. */
  title: string;
}

export const MAX_OPTIONS = 9;

/** How much text may follow the list before it stops being "the last list". */
const MAX_TRAILING_CHARS = 500;

const PLAIN = /^\s{0,3}(?:[-*+]\s+)?\**\s*(\d|[A-Ia-i])\s*[.)]\**:?\s+(\S.*)$/;
const WORD = /^\s{0,3}(?:[-*+]\s+)?\**\s*Options?\s+(\d|[A-Ia-i])\b\s*[:.)–—-]?\**:?\s*(.*)$/i;

/** Words in the prose around a list that say "pick one". */
const CHOICE_SIGNAL =
  /\b(options?|choose|choice|pick|prefer|which|alternatives?|approach(?:es)?|either|would you like|do you want|want me to|should (?:i|we)|how (?:would|do) you|your call|decide|go with|route|path)\b/i;

interface Marker {
  line: number;
  label: string;
  word: boolean;
  title: string;
}

function marker(line: string): Marker | null {
  const w = WORD.exec(line);
  if (w) return { line: 0, label: w[1]!.toUpperCase(), word: true, title: w[2] ?? '' };
  const p = PLAIN.exec(line);
  if (p) return { line: 0, label: p[1]!.toUpperCase(), word: false, title: p[2] ?? '' };
  return null;
}

function cleanTitle(s: string): string {
  const t = s
    .replace(/\*\*|__|`/g, '')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
  return t.length > 80 ? `${t.slice(0, 79).trimEnd()}…` : t;
}

/** The label that follows `label` in a sequence, or null at the end ("9" and "I" end theirs). */
function nextLabel(label: string): string | null {
  if (/^\d$/.test(label)) return label === '9' ? null : String(Number(label) + 1);
  if (label === 'I') return null;
  return String.fromCharCode(label.charCodeAt(0) + 1);
}

function startsSequence(label: string): boolean {
  return label === '1' || label === 'A';
}

/** Blank-line fenced code is skipped so a numbered line inside a code block is never a marker. */
function outsideFences(lines: string[]): boolean[] {
  const out: boolean[] = [];
  let fenced = false;
  for (const l of lines) {
    if (/^\s*(```|~~~)/.test(l)) {
      out.push(false);
      fenced = !fenced;
    } else {
      out.push(!fenced);
    }
  }
  return out;
}

/**
 * The options a reply offers, or null when it offers none. `text` is the assistant reply's
 * Markdown source.
 */
export function parseOptions(text: string): ParsedOption[] | null {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const live = outsideFences(lines);
  const marks: Marker[] = [];
  lines.forEach((l, i) => {
    if (!live[i]) return;
    const m = marker(l);
    if (m) marks.push({ ...m, line: i });
  });
  if (marks.length < 2) return null;

  // Walk the markers into runs of sequential labels; keep the last run.
  const runs: Marker[][] = [];
  let run: Marker[] = [];
  for (const m of marks) {
    const prev = run[run.length - 1];
    if (prev && nextLabel(prev.label) === m.label && m.line - prev.line <= 12) {
      run.push(m);
    } else {
      if (run.length) runs.push(run);
      run = startsSequence(m.label) ? [m] : [];
    }
  }
  if (run.length) runs.push(run);
  const last = runs.filter((r) => r.length >= 2).at(-1);
  if (!last) return null;

  const first = last[0]!;
  const end = last[last.length - 1]!;

  // Text after the last item: its continuation lines belong to it, but a long tail means the list
  // was not the end of the reply.
  const tailLines = lines.slice(end.line + 1);
  const tail = `\n${tailLines.join('\n')}`;
  const afterItem = tail.split(/\n\s*\n/).slice(1).join('\n\n');
  if (afterItem.trim().length > MAX_TRAILING_CHARS) return null;

  const explicit = last.every((m) => m.word);
  if (!explicit) {
    const intro = lines.slice(Math.max(0, first.line - 4), first.line).join('\n');
    const outro = afterItem.trim();
    const ending = text.trimEnd().replace(/[\s*_`"')\]]+$/, '');
    const signalled = CHOICE_SIGNAL.test(intro) || CHOICE_SIGNAL.test(outro) || ending.endsWith('?');
    if (!signalled) return null;
  }

  return last.map((m) => ({ label: m.label, title: cleanTitle(m.title) }));
}

/** "Option 1" / "Option A": the button caption. */
export function optionCaption(o: ParsedOption): string {
  return `Option ${o.label}`;
}

/** What a click sends. */
export function optionSendText(o: ParsedOption): string {
  return `Go with option ${o.label}.`;
}

/** What Shift+click or the pencil puts in the draft, ready to be amended. */
export function optionDraftText(o: ParsedOption): string {
  return `Go with option ${o.label} `;
}
