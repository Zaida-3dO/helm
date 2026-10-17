// The top-bar action row: one-click buttons from the user's set, and
// a "..." menu for the ones placed in the overflow. A click sends the button's text as an action
// (the draft is left alone); Helm runs nothing itself.

import { useCallback, useEffect, useRef, useState } from 'react';
import { actionOps, type ActionSet } from '../api/actions.ts';
import { useShell } from '../shell/context.ts';
import { Button, ContextMenu } from '../ui/index.ts';
import { onActionsChanged } from './changes.ts';
import { overflowButtons, topbarButtons } from './model.ts';
import { ActionError, runButton } from './send.ts';
import './actions.css';

export interface ActionRowProps {
  /** The chat in view, or null. Prompt buttons need one. */
  chatId: string | null;
}

export function ActionRow({ chatId }: ActionRowProps) {
  const shell = useShell();
  const api = shell.api;
  const onSelectChat = useCallback((id: string) => shell.select(id), [shell]);
  const [set, setSet] = useState<ActionSet | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [sending, setSending] = useState<string | null>(null);
  const moreRef = useRef<HTMLButtonElement>(null);
  const [menuAt, setMenuAt] = useState<{ x: number; y: number } | null>(null);

  const load = useCallback(() => {
    actionOps(api)
      .get()
      .then(setSet)
      .catch((e: unknown) => setNotice(`Could not load the buttons: ${String((e as Error).message ?? e)}`));
  }, [api]);
  useEffect(() => {
    load();
    return onActionsChanged(load);
  }, [load]);

  const run = useCallback(
    async (id: string) => {
      const button = set?.buttons.find((b) => b.id === id);
      if (!button || sending) return;
      setMenuAt(null);
      setNotice(null);
      setSending(id);
      try {
        await runButton(api, button, { chatId, select: onSelectChat });
      } catch (e) {
        setNotice(e instanceof ActionError ? e.message : String((e as Error).message ?? e));
      } finally {
        setSending(null);
      }
    },
    [api, chatId, onSelectChat, set, sending],
  );

  if (!set) return notice ? <div className="action-row-notice error">{notice}</div> : null;
  const top = topbarButtons(set);
  const over = overflowButtons(set);
  const needsChat = (kind: string) => kind === 'prompt' && !chatId;

  return (
    <div className="action-row" data-testid="action-row">
      {top.map((b) => (
        <Button
          key={b.id}
          className="action-button"
          data-testid="action-button"
          data-action={b.seedId ?? b.id}
          title={b.text}
          disabled={needsChat(b.kind) || sending !== null}
          onClick={() => void run(b.id)}
        >
          {b.label}
        </Button>
      ))}
      {over.length > 0 && (
        <>
          <Button
            ref={moreRef}
            className="action-button action-more"
            data-testid="action-overflow"
            aria-haspopup="menu"
            aria-expanded={menuAt !== null}
            label="More actions"
            onClick={() => {
              const r = moreRef.current?.getBoundingClientRect();
              setMenuAt(menuAt ? null : { x: r ? r.right - 190 : 0, y: r ? r.bottom + 4 : 0 });
            }}
          >
            {'…'}
          </Button>
          {menuAt && (
            <ContextMenu
              x={menuAt.x}
              y={menuAt.y}
              label="More actions"
              testId="action-menu"
              onClose={() => setMenuAt(null)}
              items={over.map((b) => ({
                id: b.seedId ?? b.id,
                label: b.label,
                disabled: needsChat(b.kind) || sending !== null,
                onSelect: () => void run(b.id),
              }))}
            />
          )}
        </>
      )}
      {notice && (
        <span className="action-row-notice error" role="status">
          {notice}
        </span>
      )}
    </div>
  );
}
