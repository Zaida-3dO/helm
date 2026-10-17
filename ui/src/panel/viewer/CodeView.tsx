// The read-only source view: CodeMirror 6
// with line numbers, virtualised rendering, the highlighter's tokens as marks, optional wrapping,
// and an initial line that is selected, marked and scrolled into view.

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

export function CodeView({ text, language, wrap, line, endLine, column, onSelection }: CodeViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const selectionCb = useRef(onSelection);
  selectionCb.current = onSelection;

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
    if (selection) {
      v.dispatch({ effects: EditorView.scrollIntoView(selection.main.head, { y: 'center' }) });
    }
    return () => {
      v.destroy();
      view.current = null;
    };
  }, [text, language, wrap, line, endLine, column]);

  return <div className="cv-host" data-testid="code-view" ref={host} />;
}
