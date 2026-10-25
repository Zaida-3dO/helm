// Ctrl+A in the transcript marks the whole chat as selected; Ctrl+C then copies all of it.
// Anything else (a click, Escape, another key) lets go of the selection.

import { useEffect, useRef, useState, type RefObject } from 'react';
import type { MessageRow, TranscriptPage } from '../api/types.ts';
import { chatToMarkdown, isEditable, loadWholeChat } from './wholeChat.ts';

export function useWholeChat(
  box: RefObject<HTMLElement | null>,
  load: (before?: string) => Promise<Pick<TranscriptPage, 'messages' | 'hasOlder' | 'olderCursor'>>,
  onNotice: (text: string) => void,
): boolean {
  const [selected, setSelected] = useState(false);
  const rows = useRef<Promise<MessageRow[]> | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const noticeRef = useRef(onNotice);
  noticeRef.current = onNotice;

  useEffect(() => {
    const inChat = (t: EventTarget | null) => {
      const el = t instanceof Element ? t : null;
      // Focus on the page itself, or anywhere inside the transcript, but not in a text field.
      return !isEditable(t) && (el === null || el === document.body || Boolean(box.current?.contains(el)));
    };
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'a' && inChat(document.activeElement) && inChat(e.target)) {
        if (document.getSelection()?.toString()) document.getSelection()?.removeAllRanges();
        e.preventDefault();
        // Start reading the history now, so Ctrl+C has it ready.
        rows.current = loadWholeChat((b) => loadRef.current(b));
        rows.current.catch(() => undefined);
        setSelected(true);
        return;
      }
      if (!selected) return;
      if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === 'c') {
        e.preventDefault();
        const pending = rows.current;
        pending
          ?.then((all) => navigator.clipboard.writeText(chatToMarkdown(all)))
          .then(() => noticeRef.current('Copied the whole chat.'))
          .catch(() => noticeRef.current('Could not copy the chat.'));
        return;
      }
      if (e.key !== 'Control' && e.key !== 'Meta' && e.key !== 'Shift' && e.key !== 'Alt') setSelected(false);
    };
    const onPointer = () => setSelected(false);
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onPointer, true);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onPointer, true);
    };
  }, [box, selected]);

  return selected;
}
