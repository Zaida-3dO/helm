// The composer's editor: Lexical in plain-text mode, holding Markdown source.
// Bold, italic, inline code and strikethrough wrap the selection in Markdown markers; Backspace
// between an empty marker pair removes both markers; undo history is per draft (the editor is
// keyed by chat, so each chat gets its own history).

import { useEffect, useImperativeHandle, type Ref } from 'react';
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
  $isRangeSelection,
  $isTextNode,
  COMMAND_PRIORITY_HIGH,
  KEY_DOWN_COMMAND,
  type EditorState,
  type LexicalEditor,
} from 'lexical';
import { wrapMarker } from './markup.ts';

export interface EditorHandle {
  getText(): string;
  setText(text: string): void;
  focus(): void;
}

export interface EditorProps {
  initialText: string;
  placeholder: string;
  onChange: (text: string) => void;
  /** Called first on every key; return true to swallow the key. */
  onKey: (e: KeyboardEvent, text: string) => boolean;
  handle: Ref<EditorHandle>;
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

function Bridge({ onKey, handle }: Pick<EditorProps, 'onKey' | 'handle'>) {
  const [editor] = useLexicalComposerContext();

  useImperativeHandle(
    handle,
    () => ({
      getText: () => editor.getEditorState().read(() => $getRoot().getTextContent()),
      setText: (text: string) => editor.update(() => $setText(text)),
      focus: () => editor.focus(),
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

export function Editor({ initialText, placeholder, onChange, onKey, handle }: EditorProps) {
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
        <Bridge onKey={onKey} handle={handle} />
      </div>
    </LexicalComposer>
  );
}
