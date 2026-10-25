// The composer's editor: Lexical in plain-text mode, holding Markdown source.
// Bold, italic, inline code and strikethrough wrap the selection in Markdown markers; Backspace
// between an empty marker pair removes both markers; undo history is per draft (the editor is
// keyed by chat, so each chat gets its own history).

import { useEffect, useImperativeHandle, useState, type ReactNode, type Ref } from 'react';
import { LexicalComposer } from '@lexical/react/LexicalComposer';
import { PlainTextPlugin } from '@lexical/react/LexicalPlainTextPlugin';
import { ContentEditable } from '@lexical/react/LexicalContentEditable';
import { HistoryPlugin } from '@lexical/react/LexicalHistoryPlugin';
import { OnChangePlugin } from '@lexical/react/LexicalOnChangePlugin';
import { LexicalErrorBoundary } from '@lexical/react/LexicalErrorBoundary';
import { useLexicalComposerContext } from '@lexical/react/LexicalComposerContext';
import {
  $createLineBreakNode,
  $createParagraphNode,
  $createTextNode,
  $getRoot,
  $getSelection,
  $isLineBreakNode,
  $isRangeSelection,
  $isTextNode,
  $setSelection,
  $createRangeSelection,
  ParagraphNode,
  TextNode,
  LineBreakNode,
  type LexicalNode,
  COMMAND_PRIORITY_HIGH,
  KEY_DOWN_COMMAND,
  type EditorState,
  type LexicalEditor,
} from 'lexical';
import { wrapMarker, liveRuns } from './markup.ts';

export interface EditorHandle {
  getText(): string;
  setText(text: string): void;
  focus(): void;
  /** True when the caret is collapsed at the very end of the draft. */
  caretAtEnd(): boolean;
}

export interface EditorProps {
  initialText: string;
  placeholder: string;
  onChange: (text: string) => void;
  /** Called first on every key; return true to swallow the key. */
  onKey: (e: KeyboardEvent, text: string) => boolean;
  handle: Ref<EditorHandle>;
  /** Muted text drawn straight after the draft's end, while the caret sits there. */
  ghost?: { base: string; completion: string } | null;
}

/** Drafts longer than this are left unstyled: styling re-reads the whole draft on every key. */
const STYLE_LIMIT = 20_000;

type Piece = { br: true } | { br?: false; text: string; style: string };

const desiredPieces = (text: string): Piece[] => {
  const out: Piece[] = [];
  liveRuns(text).forEach((runs, i) => {
    if (i > 0) out.push({ br: true });
    for (const r of runs) out.push({ text: r.text, style: r.style });
  });
  return out;
};

/** The caret offset of `point` inside `p`, counting a line break as one character. */
function offsetIn(p: ParagraphNode, nodeKey: string, offset: number, isElement: boolean): number | null {
  let n = 0;
  const kids = p.getChildren();
  if (isElement) {
    if (nodeKey !== p.getKey()) return null;
    for (let i = 0; i < Math.min(offset, kids.length); i++) n += kids[i]!.getTextContentSize();
    return n;
  }
  for (const k of kids) {
    if (k.getKey() === nodeKey) return n + offset;
    n += k.getTextContentSize();
  }
  return null;
}

/**
 * Keeps one paragraph's children split into the runs the live styling wants. Only the nodes
 * and their styles change; the characters (and so the draft) do not, and the selection is put back
 * at the same character offsets.
 */
function restyle(p: ParagraphNode) {
  const text = p.getTextContent();
  if (text.length > STYLE_LIMIT) return;
  const want = desiredPieces(text);
  const have = p.getChildren();
  const same =
    have.length === want.length &&
    have.every((k, i) => {
      const w = want[i]!;
      if (w.br) return $isLineBreakNode(k);
      return $isTextNode(k) && k.getTextContent() === w.text && k.getStyle() === w.style && k.getMode() === 'normal';
    });
  if (same) return;

  const sel = $getSelection();
  let anchor: number | null = null;
  let focus: number | null = null;
  if ($isRangeSelection(sel)) {
    anchor = offsetIn(p, sel.anchor.key, sel.anchor.offset, sel.anchor.type === 'element');
    focus = offsetIn(p, sel.focus.key, sel.focus.offset, sel.focus.type === 'element');
  }

  const made: LexicalNode[] = want.map((w) => {
    if (w.br) return $createLineBreakNode();
    const t = $createTextNode(w.text);
    if (w.style) t.setStyle(w.style);
    return t;
  });
  p.clear();
  p.append(...made);

  if (anchor !== null && focus !== null) {
    const point = (off: number) => {
      let n = 0;
      for (let i = 0; i < made.length; i++) {
        const k = made[i]!;
        const size = k.getTextContentSize();
        if ($isTextNode(k) && off <= n + size) return { key: k.getKey(), offset: off - n, type: 'text' as const };
        if (!$isTextNode(k) && off === n) return { key: p.getKey(), offset: i, type: 'element' as const };
        n += size;
      }
      return { key: p.getKey(), offset: made.length, type: 'element' as const };
    };
    const next = $createRangeSelection();
    const a = point(anchor);
    const f = point(focus);
    next.anchor.set(a.key, a.offset, a.type);
    next.focus.set(f.key, f.offset, f.type);
    $setSelection(next);
  }
}

function $setText(text: string) {
  const root = $getRoot();
  root.clear();
  const p = $createParagraphNode();
  text.split('\n').forEach((line, i) => {
    if (i > 0) p.append($createLineBreakNode());
    if (line) p.append($createTextNode(line));
  });
  root.append(p);
  p.selectEnd();
}

function caretAtEnd(): boolean {
  const sel = $getSelection();
  if (!$isRangeSelection(sel) || !sel.isCollapsed()) return false;
  const last = $getRoot().getLastDescendant();
  if (!last) return true;
  if ($isTextNode(last)) return sel.anchor.key === last.getKey() && sel.anchor.offset === last.getTextContentSize();
  const parent = last.getParent();
  return Boolean(parent) && sel.anchor.type === 'element' && sel.anchor.key === parent!.getKey() && sel.anchor.offset === parent!.getChildrenSize();
}

function Bridge({ onKey, handle }: Pick<EditorProps, 'onKey' | 'handle'>) {
  const [editor] = useLexicalComposerContext();

  useImperativeHandle(
    handle,
    () => ({
      getText: () => editor.getEditorState().read(() => $getRoot().getTextContent()),
      setText: (text: string) => editor.update(() => $setText(text)),
      focus: () => editor.focus(),
      caretAtEnd: () => editor.getEditorState().read(() => caretAtEnd()),
    }),
    [editor],
  );

  useEffect(
    () =>
      editor.registerCommand(
        KEY_DOWN_COMMAND,
        (e: KeyboardEvent) => {
          const text = editor.getEditorState().read(() => $getRoot().getTextContent());
          if (onKey(e, text)) {
            e.preventDefault();
            return true;
          }
          return markupKey(editor, e);
        },
        COMMAND_PRIORITY_HIGH,
      ),
    [editor, onKey],
  );
  return null;
}

/** Live styling for the editor's paragraphs. */
function LiveStyle() {
  const [editor] = useLexicalComposerContext();
  useEffect(() => {
    // Element transforms only fire for an element marked dirty itself, and typing marks only
    // the text node, so the text and line-break transforms find the paragraph.
    const run = (n: LexicalNode) => {
      const p = n.getParent();
      if (p instanceof ParagraphNode && !editor.isComposing()) restyle(p);
    };
    const off = [editor.registerNodeTransform(TextNode, run), editor.registerNodeTransform(LineBreakNode, run), editor.registerNodeTransform(ParagraphNode, (p) => !editor.isComposing() && restyle(p))];
    // Style the initial draft too: the transform only sees nodes marked dirty.
    editor.update(() => $getRoot().getChildren().forEach((c) => c instanceof ParagraphNode && restyle(c)));
    return () => off.forEach((f) => f());
  }, [editor]);
  return null;
}

/**
 * The completion, drawn after the draft: an invisible copy of the draft in the same styles
 * (so line wrapping matches) followed by the muted suggestion. It follows the editor's scroll.
 */
function Ghost({ ghost }: { ghost: NonNullable<EditorProps['ghost']> }) {
  const [editor] = useLexicalComposerContext();
  const [atEnd, setAtEnd] = useState(false);
  const [top, setTop] = useState(0);
  useEffect(() => {
    const check = () => setAtEnd(editor.getEditorState().read(() => caretAtEnd()));
    check();
    return editor.registerUpdateListener(check);
  }, [editor]);
  useEffect(() => {
    const root = editor.getRootElement();
    if (!root) return;
    const sync = () => setTop(root.scrollTop);
    sync();
    root.addEventListener('scroll', sync);
    return () => root.removeEventListener('scroll', sync);
  }, [editor, ghost]);
  if (!atEnd) return null;
  const lines: ReactNode[] = liveRuns(ghost.base).map((runs, i) => (
    <span key={i}>
      {i > 0 && '\n'}
      {runs.map((r, j) => (
        <span key={j} style={cssOf(r.style)}>
          {r.text}
        </span>
      ))}
    </span>
  ));
  return (
    <div className="editor-ghost" aria-hidden="true" data-testid="autocomplete-ghost">
      <div className="editor-ghost-text" style={{ transform: `translateY(${-top}px)` }}>
        <span className="editor-ghost-copy">{lines}</span>
        <span className="editor-ghost-completion">{ghost.completion}</span>
        <kbd>Tab</kbd>
      </div>
    </div>
  );
}

/** An inline style string as a React style object. */
function cssOf(style: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const decl of style.split(';')) {
    const i = decl.indexOf(':');
    if (i > 0) out[decl.slice(0, i).trim().replace(/-([a-z])/g, (_, c: string) => c.toUpperCase())] = decl.slice(i + 1).trim();
  }
  return out;
}

const MARKERS: Array<{ key: string; shift?: boolean; marker: string }> = [
  { key: 'b', marker: '**' },
  { key: 'b', shift: true, marker: '**' },
  { key: 'i', marker: '*' },
  { key: 'e', marker: '`' },
  { key: 'x', shift: true, marker: '~~' },
];

function markupKey(editor: LexicalEditor, e: KeyboardEvent): boolean {
  const mod = e.ctrlKey || e.metaKey;
  if (mod && !e.altKey) {
    const m = MARKERS.find((x) => x.key === e.key.toLowerCase() && Boolean(x.shift) === e.shiftKey);
    if (!m) return false;
    e.preventDefault();
    editor.update(() => {
      const sel = $getSelection();
      if (!$isRangeSelection(sel)) return;
      const { text, caretBack } = wrapMarker(sel.getTextContent(), m.marker);
      sel.insertText(text);
      for (let i = 0; i < caretBack; i++) sel.modify('move', true, 'character');
    });
    return true;
  }
  if (e.key === 'Backspace' && !mod && !e.altKey && !e.shiftKey) {
    let handled = false;
    editor.update(() => {
      const sel = $getSelection();
      if (!$isRangeSelection(sel) || !sel.isCollapsed()) return;
      const node = sel.anchor.getNode();
      if (!$isTextNode(node)) return;
      const off = sel.anchor.offset;
      const t = node.getTextContent();
      for (const marker of ['**', '~~', '*', '`']) {
        if (t.slice(off - marker.length, off) === marker && t.slice(off, off + marker.length) === marker) {
          node.setTextContent(t.slice(0, off - marker.length) + t.slice(off + marker.length));
          node.select(off - marker.length, off - marker.length);
          handled = true;
          return;
        }
      }
    });
    if (handled) e.preventDefault();
    return handled;
  }
  return false;
}

export function Editor({ initialText, placeholder, onChange, onKey, handle, ghost }: EditorProps) {
  const config = {
    namespace: 'helm-composer',
    onError: (e: Error) => console.error(e),
    editorState: () => $setText(initialText),
    theme: { paragraph: 'composer-p' },
  };
  return (
    <LexicalComposer initialConfig={config}>
      <div className="editor">
        <PlainTextPlugin
          contentEditable={<ContentEditable className="editor-input" data-testid="composer" aria-label="Message" />}
          placeholder={<div className="editor-placeholder">{placeholder}</div>}
          ErrorBoundary={LexicalErrorBoundary}
        />
        <HistoryPlugin />
        <OnChangePlugin
          ignoreSelectionChange
          onChange={(state: EditorState) => onChange(state.read(() => $getRoot().getTextContent()))}
        />
        <LiveStyle />
        <Bridge onKey={onKey} handle={handle} />
        {ghost && <Ghost ghost={ghost} />}
      </div>
    </LexicalComposer>
  );
}
