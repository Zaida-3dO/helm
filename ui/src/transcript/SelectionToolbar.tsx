// The selected-text toolbar: selecting text inside messages shows a small floating
// toolbar with Quote and every registered selection action (Save, Remind
// me; Side chat comes with the side-chat panel tab). A selection across messages anchors to the
// last one; at most 16,000 characters; clicks in the first 300 ms after a change are ignored.

import { useEffect, useRef, useState, type RefObject } from 'react';
import { composerApi } from '../composer/slots.ts';
import { CLICK_GUARD_MS, MAX_SELECTION_CHARS, quoteForDraft } from './selection.ts';
import { selectionActionsFor, type Selection } from './slots.ts';

interface Shown {
  sel: Selection;
  top: number;
  left: number;
}

function rowIdOf(node: Node | null): number | null {
  const el = node instanceof Element ? node : node?.parentElement;
  const row = el?.closest('[data-row-id]');
  const id = row ? Number(row.getAttribute('data-row-id')) : NaN;
  return Number.isFinite(id) ? id : null;
}

export function SelectionToolbar({ chatId, container, blocked }: { chatId: string; container: RefObject<HTMLElement | null>; blocked: boolean }) {
  const [shown, setShown] = useState<Shown | null>(null);
  const changedAt = useRef(0);

  useEffect(() => {
    const onChange = () => {
      changedAt.current = Date.now();
      const s = document.getSelection();
      const root = container.current;
      if (!s || s.isCollapsed || !root || s.rangeCount === 0) return setShown(null);
      const range = s.getRangeAt(0);
      if (!root.contains(range.commonAncestorContainer)) return setShown(null);
      const text = s.toString();
      if (!text.trim() || text.length > MAX_SELECTION_CHARS) return setShown(null);
      const a = rowIdOf(s.anchorNode);
      const b = rowIdOf(s.focusNode);
      const ids = [a, b].filter((x): x is number => x !== null).sort((x, y) => x - y);
      if (ids.length === 0) return setShown(null);
      const box = range.getBoundingClientRect();
      const base = root.getBoundingClientRect();
      setShown({
        sel: { chatId, text, rowIds: [...new Set(ids)] },
        top: box.top - base.top - 34,
        left: Math.max(0, box.left - base.left),
      });
    };
    document.addEventListener('selectionchange', onChange);
    return () => document.removeEventListener('selectionchange', onChange);
  }, [chatId, container]);

  if (!shown || blocked) return null;
  const guard = (f: () => void) => () => {
    if (Date.now() - changedAt.current < CLICK_GUARD_MS) return;
    f();
    setShown(null);
  };
  const actions = selectionActionsFor(shown.sel);
  return (
    <div className="selection-toolbar" data-testid="selection-toolbar" style={{ top: shown.top, left: shown.left }} onMouseDown={(e) => e.preventDefault()}>
      <button type="button" className="small" data-action="quote" onClick={guard(() => composerApi(chatId).insert(quoteForDraft(shown.sel.text)))}>
        Quote
      </button>
      {actions.map((a) => (
        <button key={a.id} type="button" className="small" data-action={a.id} onClick={guard(() => a.run(shown.sel))}>
          {a.label}
        </button>
      ))}
    </div>
  );
}
