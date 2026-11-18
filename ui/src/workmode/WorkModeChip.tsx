// The working-mode chip in the top bar, after the usage meter, at every bar width: the glyph
// alone at the narrow and tight tiers, the word from mid up. It shows the selected chat's
// effective mode (the app's when no chat is open), with a dot when a board override applies;
// the tooltip says where the mode was set. A click opens its popover: the app default, this
// board's own choice when the chat sits on a board, and the line about what is waiting, which
// opens Needs attention. Settings → Gatekeeper carries the same choices with the full list of
// board overrides.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useSelection, useShell } from '../shell/context.ts';
import { AnchoredDialog, PillTabs, toast, type Rect } from '../ui/index.ts';
import { WheelMark } from '../ui/status/index.ts';
import type { WorkModeView } from '../api/generated/gatekeeper/index.ts';
import type { TopBarTier } from '../topbar/fold.ts';
import { workmodeOps } from './api.ts';
import { BOARD_CHOICE_IDS, boardChoiceLabel, chipTooltip, chipWord, countsLine, modeToChoice, choiceToMode, type BoardChoiceId } from './model.ts';
import './workmode.css';

const errorText = (e: unknown) => String((e as Error).message ?? e);

export function WorkModeChip({ tier }: { tier: TopBarTier }) {
  const shell = useShell();
  const { chatId } = useSelection();
  const ops = useMemo(() => workmodeOps(shell.api), [shell.api]);
  const [view, setView] = useState<WorkModeView | null>(null);
  const [anchor, setAnchor] = useState<Rect | null>(null);
  const pill = useRef<HTMLButtonElement | null>(null);

  // The view follows the selected chat and every event that can change it or its counts: a mode
  // switch (from here, Settings or Telegram) and anything the gatekeeper decided or asked.
  useEffect(() => {
    let live = true;
    const read = () =>
      void ops
        .get(chatId ?? undefined)
        .then((v) => live && setView(v))
        .catch(() => undefined);
    read();
    const offs = [shell.bus.on('workmode', read), shell.bus.on('gate', read), shell.bus.on('attention', read)];
    return () => {
      live = false;
      offs.forEach((off) => off());
    };
  }, [ops, chatId, shell.bus]);

  if (!view) return null;
  const word = chipWord(view.effective, tier);
  const set = (scope: 'app' | 'board', mode: WorkModeView['effective'] | null, boardId?: string) => {
    ops
      .set({ scope, mode, ...(boardId ? { boardId } : {}) })
      .then((v) => setView(v))
      .catch((e: unknown) => toast.show({ kind: 'error', text: errorText(e) }));
  };
  const openPopover = () => {
    const r = pill.current?.getBoundingClientRect();
    if (r) setAnchor({ left: r.left, top: r.top, width: r.width, height: r.height });
  };
  return (
    <>
      <button
        ref={pill}
        type="button"
        className="workmode-chip"
        data-testid="workmode-chip"
        data-mode={view.effective}
        data-board={view.source === 'board' ? 'true' : undefined}
        title={chipTooltip(view)}
        aria-label={`Working mode: ${chipTooltip(view)}`}
        onClick={openPopover}
      >
        <WheelMark size={13} />
        {word}
        {view.source === 'board' && <span className="workmode-board-dot" aria-hidden="true">· board</span>}
      </button>
      {anchor && (
        <AnchoredDialog title="Working mode" testId="workmode-pop" anchor={anchor} onClose={() => setAnchor(null)}>
          <p className="workmode-pop-note">
            Run to completion: chats carry on and decide the small things themselves, keeping each choice for you to review. Check in with me: a chat stops and
            asks when a choice matters. Helm checks every stop in run mode, and anything that cannot be undone always comes to you.
          </p>
          <PillTabs<'run' | 'check_in'>
            label="Default for every chat"
            value={view.app}
            onChange={(id) => set('app', id)}
            testId="workmode-app"
            tabs={[
              { id: 'check_in', label: 'Check in with me' },
              { id: 'run', label: 'Run to completion' },
            ]}
          />
          {view.board && (
            <div className="workmode-pop-board">
              <PillTabs<BoardChoiceId>
                label={`This board: ${view.board.name}`}
                value={modeToChoice(view.board.mode)}
                onChange={(id) => set('board', choiceToMode(id), view.board!.id)}
                testId="workmode-board"
                tabs={BOARD_CHOICE_IDS.map((id) => ({ id, label: boardChoiceLabel(choiceToMode(id)) }))}
              />
            </div>
          )}
          <p className="workmode-pop-counts">
            <button
              type="button"
              className="workmode-counts-link"
              data-testid="workmode-counts"
              onClick={() => {
                setAnchor(null);
                shell.bus.dispatch({ kind: 'shell_open_attention' });
              }}
            >
              {countsLine(view.counts)}
            </button>
          </p>
        </AnchoredDialog>
      )}
    </>
  );
}
