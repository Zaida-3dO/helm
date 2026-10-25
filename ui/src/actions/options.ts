// Inline option buttons and the answer popup: find the choices an assistant
// reply offers, so each can be answered with one click.
//
// A list counts as choices only when it is the reply's last list, has 2 to 9 sequential markers
// (`1.`, `1)`, `Option 1`, `A)`, `**A.**`, `Option A:`) and either uses the explicit word "Option"
// or the reply signals that a choice is wanted. Numbered steps, a changelog or a plan that merely
// happens to be numbered must not grow buttons, so plain markers need that signal.

export interface ParsedOption {
  /** "1", "2", ... or "A", "B", ... */
  label: string;
  /** The item in full: its continuation lines too, cleaned of markdown, with any "(Recommended)" tag taken out. */
  text: string;
  /** A short form of the text, for a chip. */
  short: string;
  /** The reply marks this option as the recommended or default one. */
  recommended: boolean;
}

/** A reply's question and the options it offers, as the answer popup shows them. */
export interface ReplyQuestion {
  /** The prose around the list, as Markdown: what is being asked. */
  question: string;
  options: ParsedOption[];
  /** The question says more than one answer may be chosen. */
  multi: boolean;
}

export const MAX_OPTIONS = 9;

/** How much text may follow the list before it stops being "the last list". */
const MAX_TRAILING_CHARS = 500;

/** The question shown in the popup: the prose just before the list plus a short closing line. */
const MAX_QUESTION_CHARS = 700;

/** Longest chip text before it is cut. */
const SHORT_CHARS = 36;

const PLAIN = /^\s{0,3}(?:[-*+]\s+)?\**\s*(\d|[A-Ia-i])\s*[.)]\**:?\s+(\S.*)$/;
const WORD = /^\s{0,3}(?:[-*+]\s+)?\**\s*Options?\s+(\d|[A-Ia-i])\b\s*[:.)–—-]?\**:?\s*(.*)$/i;

/** Words in the prose around a list that say "pick one". */
const CHOICE_SIGNAL =
  /\b(options?|choose|choice|pick|prefer|which|alternatives?|approach(?:es)?|either|would you like|do you want|want me to|should (?:i|we)|how (?:would|do) you|your call|decide|go with|route|path)\b/i;

/** A tag that makes an option the recommended one: "(Recommended)", "[default]", "- recommended". */
const RECOMMENDED_TAG = /\s*[([](?:recommended|default|my pick|suggested)[)\]]\s*|\s+[-–—]\s*(?:recommended|default)\s*$|\s*\*\*recommended\*\*\s*/i;
const RECOMMENDED_WORD = /\b(?:recommended|my pick)\b/i;

/** Wording of a question that allows more than one answer. */
const MULTI_SIGNAL = /\b(?:all that apply|(?:select|choose|pick) (?:all|any|multiple|several|more than one)|one or more|any combination|as many as)\b/i;

interface Marker {
  line: number;
  label: string;
  word: boolean;
  /** The marker line's own text, raw. */
  title: string;
}

function marker(line: string): Marker | null {
  const w = WORD.exec(line);
  if (w) return { line: 0, label: w[1]!.toUpperCase(), word: true, title: w[2] ?? '' };
  const p = PLAIN.exec(line);
  if (p) return { line: 0, label: p[1]!.toUpperCase(), word: false, title: p[2] ?? '' };
  return null;
}

function cleanText(s: string): string {
  return s
    .replace(/\*\*|__|`/g, '')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
}

/** The chip text: the item up to its first clause break, cut at a word if still long. */
function shortOf(text: string): string {
  const head = text.split(/:\s|\s[-–—]\s|\.\s|\s\(/)[0]?.trim() || text;
  if (head.length <= SHORT_CHARS) return head;
  const cut = head.slice(0, SHORT_CHARS - 1);
  const at = cut.lastIndexOf(' ');
  return `${(at > 12 ? cut.slice(0, at) : cut).trimEnd()}…`;
}

function makeOption(label: string, raw: string): ParsedOption {
  const recommended = RECOMMENDED_TAG.test(raw) || RECOMMENDED_WORD.test(raw);
  const text = cleanText(raw.replace(RECOMMENDED_TAG, ' '));
  return { label, text, short: shortOf(text), recommended };
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

interface FoundList {
  options: ParsedOption[];
  /** Line index of the first marker. */
  first: number;
  /** Line index just past the last item and its continuation lines. */
  end: number;
  lines: string[];
}

/**
 * The lines that belong to an item after its marker line: up to the next marker, running over
 * indented lines (and, for an item that is not the last, any line) but stopping at a blank line
 * that is followed by something outside the item.
 */
function itemEnd(lines: string[], from: number, limit: number, last: boolean): number {
  let i = from;
  while (i < limit) {
    const l = lines[i]!;
    if (l.trim() === '') {
      let j = i + 1;
      while (j < limit && lines[j]!.trim() === '') j++;
      if (j >= limit || !/^\s{2,}\S/.test(lines[j]!)) return i;
      i = j;
      continue;
    }
    if (last && !/^\s{2,}\S/.test(l)) return i;
    i++;
  }
  return i;
}

function findList(text: string): FoundList | null {
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
  const endMark = last[last.length - 1]!;

  // Text after the last item: its continuation lines belong to it, but a long tail means the list
  // was not the end of the reply.
  const tailLines = lines.slice(endMark.line + 1);
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

  let end = endMark.line + 1;
  const options = last.map((m, k) => {
    const next = last[k + 1];
    const isLast = !next;
    const stop = itemEnd(lines, m.line + 1, next ? next.line : lines.length, isLast);
    if (isLast) end = stop;
    const more = lines
      .slice(m.line + 1, stop)
      .map((l) => l.replace(/^\s*(?:[-*+]\s+)?/, ''))
      .filter((l) => l !== '');
    return makeOption(m.label, [m.title, ...more].join(' '));
  });
  return { options, first: first.line, end, lines };
}

/**
 * The options a reply offers, or null when it offers none. `text` is the assistant reply's
 * Markdown source.
 */
export function parseOptions(text: string): ParsedOption[] | null {
  return findList(text)?.options ?? null;
}

/** The last paragraph before the list (with the one before it when that is only a lead-in word). */
function introOf(lines: string[], first: number): string {
  const blocks = lines
    .slice(0, first)
    .join('\n')
    .split(/\n\s*\n/)
    .map((b) => b.trim())
    .filter((b) => b !== '');
  const last = blocks.at(-1) ?? '';
  const prior = blocks.at(-2);
  return last.length < 30 && prior ? `${prior}\n\n${last}` : last;
}

/**
 * The question a reply asks and the options it offers: what the answer popup shows. The question
 * is the paragraph before the list plus a short closing line after it ("Which do you prefer?").
 */
export function parseReplyQuestion(text: string): ReplyQuestion | null {
  const found = findList(text);
  if (!found) return null;
  const intro = introOf(found.lines, found.first);
  const outro = found.lines.slice(found.end).join('\n').trim();
  let question = [intro, outro].filter((s) => s !== '').join('\n\n');
  if (question.length > MAX_QUESTION_CHARS) question = `…${question.slice(-MAX_QUESTION_CHARS).trimStart()}`;
  return { question, options: found.options, multi: MULTI_SIGNAL.test(question) };
}

/** The option the popup starts with chosen: the one the reply marks as recommended or default. */
export function defaultOption(options: readonly ParsedOption[]): ParsedOption | null {
  return options.find((o) => o.recommended) ?? null;
}

/** The button's label: "Answer question", or "Answer 3 questions". */
export function answerLabel(count: number): string {
  return count > 1 ? `Answer ${count} questions` : 'Answer question';
}

/** The chip's caption: the option's short text, or "Option N" when it has none. */
export function optionCaption(o: ParsedOption): string {
  return o.short || `Option ${o.label}`;
}

function sentence(text: string): string {
  return /[.!?…]$/.test(text) ? text : `${text}.`;
}

function listLabels(labels: string[]): string {
  return labels.length < 2 ? (labels[0] ?? '') : `${labels.slice(0, -1).join(', ')} and ${labels.at(-1)}`;
}

/**
 * What an answer sends: the chosen options named by label and written out, then anything typed
 * under "Other". Typed text alone is sent as it is.
 */
export function answerText(picked: readonly ParsedOption[], other = ''): string {
  const free = other.trim();
  if (picked.length === 0) return free;
  const head = `Go with option${picked.length > 1 ? 's' : ''} ${listLabels(picked.map((o) => o.label))}`;
  const body = picked.map((o) => o.text).filter((t) => t !== '');
  const main = body.length === 0 ? `${head}.` : `${head}: ${sentence(body.join('; '))}`;
  return free ? `${main} Also: ${sentence(free)}` : main;
}

/** What a click sends. */
export function optionSendText(o: ParsedOption): string {
  return answerText([o]);
}

/** What Shift+click or the pencil puts in the draft, ready to be amended. */
export function optionDraftText(o: ParsedOption): string {
  return `Go with option ${o.label}${o.text ? `: ${o.text}` : ''} `;
}
