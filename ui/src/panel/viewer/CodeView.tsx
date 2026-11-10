// The read-only source view: CodeMirror 6
// with line numbers, virtualised rendering, the highlighter's tokens as marks, optional wrapping,
// and an initial line that is selected, marked and scrolled into view. When the text changes under
// it (the file was edited) the first visible line stays at the same offset, and the view reports
// its first visible line so a tab can come back to where it was left.

import { EditorSelection, EditorState } from '@codemirror/state';
import type { Extension } from '@codemirror/state';
import { Decoration, EditorView, lineNumbers } from '@codemirror/view';
import { useEffect, useRef } from 'react';
import { tokenRanges } from './tokens.ts';

export interface CodeViewProps {
  text: string;
  language?: string;
  wrap: boolean;
  /** 1-based line to show and mark. */
  line?: number;
  /** The last line of a marked range (inclusive). */
  endLine?: number;
  column?: number;
  /** The current selection as 1-based lines, for the side panel context. */
  onSelection?: (sel: { startLine: number; endLine: number; text: string } | null) => void;
  /** Which file this is: a different file starts from its own top, not the last file's place. */
  fileKey?: string;
  /** The first visible line to come back to when the view is first built. */
  topLine?: number;
  /** The first visible line as the person scrolls. */
  onTopLine?: (line: number) => void;
}

/** Where the reading position was: a line and how far below the top of the view its top edge sat. */
interface Anchor {
  fileKey: string | undefined;
  /** The marked target the view was built for; a new target means a new place to go to. */
  target: string;
  line: number;
  offset: number;
}

function firstVisible(v: EditorView): { line: number; offset: number } {
  const top = v.scrollDOM.scrollTop;
  const block = v.lineBlockAtHeight(top);
  return { line: v.state.doc.lineAt(block.from).number, offset: top - block.top };
}

const theme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'var(--bg)', color: 'var(--text)' },
  '.cm-scroller': {
    fontFamily: 'var(--font-mono)',
    fontSize: 'var(--code-font-size, 12px)',
    lineHeight: '1.6',
  },
  '.cm-gutters': { backgroundColor: 'var(--bg)', color: 'var(--muted)', border: 'none' },
  '.cm-lineNumbers .cm-gutterElement': { minWidth: 'max(44px, calc(5ch + 12px))', paddingRight: '8px' },
  '.cm-content': { caretColor: 'var(--text)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection': {
    backgroundColor: 'color-mix(in srgb, var(--accent) 28%, transparent)',
  },
  '.cv-target-line': {
    backgroundColor: 'var(--hover)',
    boxShadow: 'inset 3px 0 0 var(--accent)',
  },
});

function decorations(text: string, language: string | undefined, line: number | undefined, endLine: number | undefined, state: EditorState) {
  const marks = tokenRanges(text, language).map((r) => Decoration.mark({ class: r.className }).range(r.from, r.to));
  if (line && line >= 1 && line <= state.doc.lines) {
    const last = Math.min(state.doc.lines, Math.max(line, endLine ?? line));
    for (let n = line; n <= last; n++) marks.push(Decoration.line({ class: 'cv-target-line' }).range(state.doc.line(n).from));
  }
  return Decoration.set(marks, true);
}

export function CodeView({ text, language, wrap, line, endLine, column, onSelection, fileKey, topLine, onTopLine }: CodeViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const selectionCb = useRef(onSelection);
  selectionCb.current = onSelection;
  const topCb = useRef(onTopLine);
  topCb.current = onTopLine;
  // Left by the view that is being replaced, picked up by the next one.
  const anchor = useRef<Anchor | null>(null);
  // The saved place is used once, by the first view this component builds.
  const savedTop = useRef(topLine);

  useEffect(() => {
    const parent = host.current;
    if (!parent) return;
    const base = EditorState.create({ doc: text });
    const extensions: Extension[] = [
      EditorState.readOnly.of(true),
      lineNumbers(),
      theme,
      EditorView.decorations.of(decorations(text, language, line, endLine, base)),
      EditorView.updateListener.of((u) => {
        if (!u.selectionSet || !selectionCb.current) return;
        const r = u.state.selection.main;
        if (r.empty) return selectionCb.current(null);
        selectionCb.current({
          startLine: u.state.doc.lineAt(r.from).number,
          endLine: u.state.doc.lineAt(r.to).number,
          text: u.state.sliceDoc(r.from, r.to),
        });
      }),
    ];
    if (wrap) extensions.push(EditorView.lineWrapping);
    let selection: EditorSelection | undefined;
    if (line && line >= 1 && line <= base.doc.lines) {
      const l = base.doc.line(line);
      const at = Math.min(l.to, l.from + Math.max(0, (column ?? 1) - 1));
      selection = EditorSelection.single(at);
    }
    const state = EditorState.create({ doc: text, extensions, selection });
    const v = new EditorView({ state, parent });
    view.current = v;
    const target = `${line ?? ''}:${endLine ?? ''}:${column ?? ''}`;
    const keep = anchor.current && anchor.current.fileKey === fileKey && anchor.current.target === target ? anchor.current : null;
    anchor.current = null;
    const place = keep ? { line: keep.line, offset: keep.offset } : savedTop.current ? { line: savedTop.current, offset: 0 } : null;
    savedTop.current = undefined;
    let frame = 0;
    if (place) {
      // An edit or a return to a saved place: put the same line back where it was, not at the target.
      const n = Math.min(Math.max(1, place.line), v.state.doc.lines);
      const apply = () => {
        v.scrollDOM.scrollTop = v.lineBlockAt(v.state.doc.line(n).from).top + place.offset;
      };
      apply();
      frame = requestAnimationFrame(apply);
    } else if (selection) {
      v.dispatch({ effects: EditorView.scrollIntoView(selection.main.head, { y: 'center' }) });
    }
    let timer: ReturnType<typeof setTimeout> | null = null;
    const onScroll = () => {
      if (timer || !topCb.current) return;
      timer = setTimeout(() => {
        timer = null;
        if (view.current === v) topCb.current?.(firstVisible(v).line);
      }, 250);
    };
    v.scrollDOM.addEventListener('scroll', onScroll, { passive: true });
    return () => {
      if (timer) clearTimeout(timer);
      if (frame) cancelAnimationFrame(frame);
      v.scrollDOM.removeEventListener('scroll', onScroll);
      const at = firstVisible(v);
      anchor.current = { fileKey, target, line: at.line, offset: at.offset };
      v.destroy();
      view.current = null;
      // A rebuilt or removed view has no selection left to report.
      selectionCb.current?.(null);
    };
  }, [text, language, wrap, line, endLine, column, fileKey]);

  return <div className="cv-host" data-testid="code-view" ref={host} />;
}
