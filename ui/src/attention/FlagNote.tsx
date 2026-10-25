// The idle check's note inside a chat: when the chat you opened is flagged as quiet but possibly
// unfinished, the reason sits just above the composer with Snooze and Mark complete. It follows the
// host's attention summary, so it goes away when the flag clears (new activity, snoozed, marked
// complete, or the check switched off).

import { useEffect, useRef, useState } from 'react';
import type { ChatOps } from '../api/ops.ts';
import type { AttentionSummary } from '../api/types.ts';
import { useShell } from '../shell/context.ts';
import { Button, ContextMenu, toast, type MenuItem } from '../ui/index.ts';
import { IconCheck, IconClock, IconFlag } from '../ui/icons.tsx';
import { flagFor, snoozeChoices } from './model.ts';
import './attention.css';

/** The newest attention summary, kept by revision. */
function useSummary(ops: ChatOps): AttentionSummary | null {
  const shell = useShell();
  const [summary, setSummary] = useState<AttentionSummary | null>(null);
  useEffect(() => {
    let live = true;
    const apply = (s: AttentionSummary) => live && setSummary((was) => (was && was.revision > s.revision ? was : s));
    const off = shell.bus.on('attention', (e) => apply(e.summary as AttentionSummary));
    ops.attention().then(apply).catch(() => undefined);
    return () => {
      live = false;
      off();
    };
  }, [shell, ops]);
  return summary;
}

export function FlagNote({ chatId, ops }: { chatId: string; ops: ChatOps }) {
  const summary = useSummary(ops);
  const row = flagFor(summary, chatId);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const was = useRef(false);
  // An action that worked hides the note at once; the host's next summary confirms it. A later
  // flag on the same chat (after new activity) must show again.
  useEffect(() => {
    if (!row && was.current) setDone(false);
    was.current = Boolean(row);
  }, [row]);

  if (!row || done) return null;
  const act = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
      setDone(true);
    } catch {
      toast.show({ kind: 'error', text: 'Could not update this flag.' });
    } finally {
      setBusy(false);
    }
  };
  const items: MenuItem[] = snoozeChoices(new Date()).map((c) => ({
    id: `snooze:${c.id}`,
    label: c.label,
    shortcut: c.when,
    onSelect: () => void act(() => ops.snooze(chatId, c.until)),
  }));
  return (
    <div className="flag-note" data-testid="flag-note" aria-busy={busy ? true : undefined}>
      <div className="flag-note-label">
        <IconFlag size={11} filled />
        Needs attention
      </div>
      <p className="flag-note-reason" data-testid="flag-note-reason">
        {row.reason}
      </p>
      <div className="flag-note-actions">
        <Button
          variant="ghost"
          aria-haspopup="menu"
          disabled={busy}
          data-testid="flag-note-snooze"
          onClick={(e) => {
            const b = e.currentTarget.getBoundingClientRect();
            setMenu({ x: b.left, y: b.top - 4 });
          }}
        >
          <IconClock size={13} />
          Snooze
        </Button>
        <Button variant="ghost" disabled={busy} data-testid="flag-note-complete" onClick={() => void act(() => ops.markAddressed(chatId))}>
          <IconCheck size={13} />
          Mark complete
        </Button>
      </div>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={items} label="Snooze this flag" onClose={() => setMenu(null)} testId="flag-note-menu" />}
    </div>
  );
}
