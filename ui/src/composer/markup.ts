// Markdown wrapping for the editor's formatting keys. Pure.

/** Wrap `selected` in `marker`. With nothing selected the caret goes between the markers. */
export function wrapMarker(selected: string, marker: string): { text: string; caretBack: number } {
  if (!selected) return { text: marker + marker, caretBack: marker.length };
  // Already wrapped: unwrap.
  if (selected.length >= marker.length * 2 && selected.startsWith(marker) && selected.endsWith(marker)) {
    return { text: selected.slice(marker.length, selected.length - marker.length), caretBack: 0 };
  }
  return { text: marker + selected + marker, caretBack: 0 };
}

// ---- Live styling -------------------------------------------------------------------------
//
// The editor holds Markdown source, and styles it as it is typed: headings, bold, italic, inline
// code, strikethrough, links, lists and fenced code, with the marker characters dimmed. The draft
// text never changes, only how runs of it are drawn, so `getTextContent()` stays the source.

/** A run of one line's text and the inline CSS it is drawn with ('' for plain). */
export interface StyledRun {
  text: string;
  style: string;
}

const MARKER = 'color:var(--muted)';
const MONO = 'font-family:var(--font-mono,monospace);font-size:.93em';
const CODE = `${MONO};background:var(--surface-alt);border-radius:4px`;
const CHIP = 'background:color-mix(in srgb, var(--accent) 12%, transparent);border-radius:5px;font-weight:550';
const HEADING = ['font-size:1.32em', 'font-size:1.2em', 'font-size:1.1em', 'font-size:1.04em', 'font-size:1em', 'font-size:1em'];

const INLINE =
  /(`+)([^`\n]+?)\1|\*\*([^*\n]+?)\*\*|~~([^~\n]+?)~~|(?<![\w*])\*([^*\s][^*\n]*?)\*(?![\w*])|(?<![\w_])_([^_\s][^_\n]*?)_(?![\w_])|https?:\/\/[^\s<>)\]]*[^\s<>)\].,;:!?'"]/g;

const joinStyle = (base: string, extra: string) => (base ? `${base};${extra}` : extra);

function push(runs: StyledRun[], text: string, style: string) {
  if (!text) return;
  const last = runs[runs.length - 1];
  if (last && last.style === style) last.text += text;
  else runs.push({ text, style });
}

/** Inline spans inside one line (or the rest of it after a heading or list marker). */
function inlineRuns(text: string, base: string, runs: StyledRun[]) {
  let at = 0;
  for (const m of text.matchAll(INLINE)) {
    const i = m.index!;
    push(runs, text.slice(at, i), base);
    const whole = m[0];
    if (m[1] !== undefined) {
      const tick = m[1];
      push(runs, tick, joinStyle(base, MARKER));
      push(runs, m[2]!, joinStyle(base, CODE));
      push(runs, tick, joinStyle(base, MARKER));
    } else if (m[3] !== undefined) {
      push(runs, '**', joinStyle(base, MARKER));
      push(runs, m[3], joinStyle(base, 'font-weight:650'));
      push(runs, '**', joinStyle(base, MARKER));
    } else if (m[4] !== undefined) {
      push(runs, '~~', joinStyle(base, MARKER));
      push(runs, m[4], joinStyle(base, 'text-decoration:line-through'));
      push(runs, '~~', joinStyle(base, MARKER));
    } else if (m[5] !== undefined || m[6] !== undefined) {
      push(runs, whole[0]!, joinStyle(base, MARKER));
      push(runs, (m[5] ?? m[6])!, joinStyle(base, 'font-style:italic'));
      push(runs, whole[0]!, joinStyle(base, MARKER));
    } else {
      push(runs, whole, joinStyle(base, 'color:var(--accent);text-decoration:underline'));
    }
    at = i + whole.length;
  }
  push(runs, text.slice(at), base);
}

/** Every line of `text` as styled runs. Joining each line's runs gives the line back exactly. */
export function liveRuns(text: string, commands?: ReadonlySet<string>): StyledRun[][] {
  let fenced = false;
  return text.split('\n').map((line, n) => {
    const runs: StyledRun[] = [];
    if (/^\s*(```|~~~)/.test(line)) {
      fenced = !fenced;
      push(runs, line, `${MARKER};${MONO}`);
      return runs;
    }
    if (fenced) {
      push(runs, line, MONO);
      return runs;
    }
    const heading = /^(#{1,6})(\s+)(.*)$/.exec(line);
    if (heading) {
      const base = `${HEADING[heading[1]!.length - 1]};font-weight:650`;
      push(runs, heading[1]! + heading[2]!, `${MARKER};font-size:inherit`);
      inlineRuns(heading[3]!, base, runs);
      return runs;
    }
    const item = /^(\s*)([-*+]|\d{1,9}[.)])(\s+)(.*)$/.exec(line);
    if (item) {
      push(runs, item[1]!, '');
      push(runs, item[2]!, 'color:var(--accent);font-weight:600');
      push(runs, item[3]!, '');
      inlineRuns(item[4]!, '', runs);
      return runs;
    }
    const quote = /^(>\s?)(.*)$/.exec(line);
    if (quote) {
      push(runs, quote[1]!, MARKER);
      inlineRuns(quote[2]!, 'color:var(--secondary)', runs);
      return runs;
    }
    // A draft that opens with a known command or skill shows it as a chip.
    const command = n === 0 && commands ? /^\/([A-Za-z0-9_:-]+)(?=\s|$)/.exec(line) : null;
    if (command && commands!.has(command[1]!.toLowerCase())) {
      push(runs, command[0], CHIP);
      inlineRuns(line.slice(command[0].length), '', runs);
      return runs;
    }
    inlineRuns(line, '', runs);
    return runs;
  });
}
