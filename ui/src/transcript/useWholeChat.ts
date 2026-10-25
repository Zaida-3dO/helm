// Ctrl+A in the transcript marks the whole chat as selected; Ctrl+C then copies all of it.
// Anything else (a click, Escape, another key) lets go of the selection.
//
// A side chat can be open beside the main chat, and each has a view. With focus on the page itself
// only one of them takes the keys: the one last clicked in, or the first opened when none was.

import { useEffect, useRef, useState, type RefObject } from 'react';
import type { MessageRow, TranscriptPage } from '../api/types.ts';
import { chatToMarkdown, copiedNotice, isEditable, loadWholeChat } from './wholeChat.ts';

const views: symbol[] = [];
let lastUsed: symbol | null = null;

/** Whether `me` is the view that takes a key pressed with focus on the page itself. */
export function ownsPage(me: symbol, registered: readonly symbol[], used: symbol | null): boolean {
  return used !== null && registered.includes(used) ? used === me : registered[0] === me;
}

export function useWholeChat(
  box: RefObject<HTMLElement | null>,
  load: (before?: string) => Promise<Pick<TranscriptPage, 'messages' | 'hasOlder' | 'olderCursor'>>,
  onNotice: (text: string) => void,
): boolean {
  const [selected, setSelected] = useState(false);
  const rows = useRef<Promise<{ rows: MessageRow[]; capped: boolean }> | null>(null);
  const me = useRef<symbol>(Symbol('chat view'));
  const loadRef = useRef(load);
  loadRef.current = load;
  const noticeRef = useRef(onNotice);
  noticeRef.current = onNotice;

  useEffect(() => {
    const id = me.current;
    views.push(id);
    return () => {
      views.splice(views.indexOf(id), 1);
      if (lastUsed === id) lastUsed = null;
    };
  }, []);

  useEffect(() => {
    // Focus on the page itself, or anywhere inside this transcript, but not in a text field.
    const inChat = (t: EventTarget | null) => {
      const el = t instanceof Element ? t : null;
      if (isEditable(t)) return false;
      if (el !== null && el !== document.body) return Boolean(box.current?.contains(el));
      return ownsPage(me.current, views, lastUsed);
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
        // Focus may have moved since Ctrl+A: then this is an ordinary copy, not ours.
        if (!inChat(document.activeElement)) {
          setSelected(false);
          return;
        }
        e.preventDefault();
        rows.current
          ?.then(({ rows: all, capped }) => navigator.clipboard.writeText(chatToMarkdown(all)).then(() => noticeRef.current(copiedNotice(capped))))
          .catch(() => noticeRef.current('Could not copy the chat.'));
        return;
      }
      if (e.key !== 'Control' && e.key !== 'Meta' && e.key !== 'Shift' && e.key !== 'Alt') setSelected(false);
    };
    const onPointer = (e: Event) => {
      if (box.current?.contains(e.target as Node)) lastUsed = me.current;
      setSelected(false);
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('pointerdown', onPointer, true);
    return () => {
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('pointerdown', onPointer, true);
    };
  }, [box, selected]);

  return selected;
}
