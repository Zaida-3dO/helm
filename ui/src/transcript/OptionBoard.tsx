// An option board the agent put in the chat (app_offer_options): a grid of tiles, each with an
// optional picture, a label and a line of detail. A tap (or a number key) chooses; a board that
// allows several shows a Send button; "Other" opens a text field. The choice goes out as the user's
// own message (sendActionText), then the host records it, so the card shows what was chosen and
// stops taking taps. It never blocks the agent: a tap while a turn runs steers in like any message.

import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { ApiError } from '../api/client.ts';
import { cardOps, type OptionBoard as Board } from '../api/ops.ts';
import { sendActionText } from '../actions/send.ts';
import { useShell } from '../shell/context.ts';
import { IconCheck } from '../ui/icons.tsx';
import { chosenText, optionForKey, optionMessage } from './cards.ts';
import { useShownImage } from './shownImages.ts';
import './cards.css';

type Fetched = { state: 'loading' } | { state: 'ready'; board: Board } | { state: 'failed' };

const OTHER = -1;

function Tile({ chatId, board, index, selected, disabled, onPick }: { chatId: string; board: Board; index: number; selected: boolean; disabled: boolean; onPick: () => void }) {
  const option = board.options[index]!;
  const image = useShownImage(chatId, board.boardId, option.index, true);
  return (
    <button
      type="button"
      className={`ob-tile${selected ? ' on' : ''}`}
      role={board.multiSelect ? 'checkbox' : 'radio'}
      aria-checked={selected}
      disabled={disabled}
      data-testid="option-tile"
      data-label={option.label}
      onClick={onPick}
    >
      {option.hasImage && (
        <span className="ob-tile-image" aria-hidden="true">
          {image.state === 'ready' ? <img src={image.url} alt="" draggable={false} /> : null}
        </span>
      )}
      <span className="ob-tile-text">
        <span className="ob-tile-label">
          {index < 9 && <span className="ob-key">{index + 1}</span>}
          {option.label}
        </span>
        {option.detail && <span className="ob-tile-detail muted">{option.detail}</span>}
      </span>
      {selected && <IconCheck size={14} className="ob-tile-check" />}
    </button>
  );
}

export function OptionBoardCard({ chatId, boardId }: { chatId: string; boardId: string }) {
  const { api } = useShell();
  const ops = useMemo(() => cardOps(api), [api]);
  const [fetched, setFetched] = useState<Fetched>({ state: 'loading' });
  const [picked, setPicked] = useState<number[]>([]);
  const [other, setOther] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    setFetched({ state: 'loading' });
    ops
      .board(chatId, boardId)
      .then((board) => live && setFetched({ state: 'ready', board }))
      .catch(() => live && setFetched({ state: 'failed' }));
    return () => {
      live = false;
    };
  }, [ops, chatId, boardId]);

  const board = fetched.state === 'ready' ? fetched.board : null;
  const answered = board?.answer != null;

  // A board that has just appeared takes keyboard focus when nothing else has it, so the number
  // keys work straight away; a reader typing in the composer is never interrupted.
  const fresh = board !== null && board.answer === null && Date.now() - board.createdAt < 120_000;
  useEffect(() => {
    if (!fresh) return;
    const active = document.activeElement;
    if (!active || active === document.body) root.current?.focus({ preventScroll: true });
  }, [fresh]);

  if (fetched.state === 'loading') return <div className="option-board option-board-wait" data-testid="option-board" data-state="loading" aria-busy="true" />;
  if (!board) {
    return (
      <div className="option-board muted" data-testid="option-board" data-state="failed">
        This choice is no longer available.
      </div>
    );
  }

  const send = async (labels: string[], text?: string) => {
    if (busy || board.answer) return;
    setBusy(true);
    setError(null);
    try {
      await sendActionText(api, chatId, optionMessage(board, labels, text));
    } catch (e) {
      setBusy(false);
      setError(`Your choice was not sent: ${e instanceof Error ? e.message : String(e)}`);
      return;
    }
    try {
      setFetched({ state: 'ready', board: await ops.answer(chatId, boardId, text !== undefined ? { other: text.trim() } : { labels }) });
    } catch (e) {
      // The message is out already. A board answered elsewhere just shows what was recorded.
      if (e instanceof ApiError && e.code === 'conflict') {
        await ops.board(chatId, boardId).then(
          (b) => setFetched({ state: 'ready', board: b }),
          () => undefined,
        );
      } else {
        setError('Your choice was sent, but the card could not record it.');
      }
    } finally {
      setBusy(false);
    }
  };

  const labelOf = (i: number) => board.options[i]!.label;
  const pick = (i: number) => {
    if (answered || busy) return;
    if (i === OTHER) {
      setOther((o) => (o === null ? '' : null));
      return;
    }
    if (board.multiSelect) setPicked((p) => (p.includes(i) ? p.filter((x) => x !== i) : [...p, i]));
    else {
      setPicked([i]);
      void send([labelOf(i)]);
    }
  };
  const submitMulti = () => {
    const labels = [...picked].sort((a, b) => a - b).map(labelOf);
    if (labels.length > 0) void send(labels);
  };
  const submitOther = () => {
    if (other?.trim()) void send([], other.trim());
  };
  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (answered || e.altKey || e.ctrlKey || e.metaKey || e.target instanceof HTMLInputElement) return;
    const i = optionForKey(e.key, board.options.length);
    if (i === null) return;
    e.preventDefault();
    pick(i);
  };
  const chosen = board.answer ? chosenText(board.answer) : null;
  const selected = (i: number) => (board.answer ? board.answer.labels.includes(labelOf(i)) : picked.includes(i));

  return (
    <div
      ref={root}
      className={`option-board${answered ? ' answered' : ''}`}
      data-testid="option-board"
      data-state={answered ? 'answered' : 'open'}
      data-board={board.boardId}
      tabIndex={answered ? undefined : 0}
      aria-label={board.question}
      onKeyDown={onKeyDown}
    >
      <div className="ob-question">{board.question}</div>
      <div className="ob-grid" role={board.multiSelect ? 'group' : 'radiogroup'} aria-label={board.question}>
        {board.options.map((o, i) => (
          <Tile key={o.index} chatId={chatId} board={board} index={i} selected={selected(i)} disabled={answered || busy} onPick={() => pick(i)} />
        ))}
        {board.allowOther && !answered && (
          <button type="button" className={`ob-tile ob-other${other !== null ? ' on' : ''}`} aria-pressed={other !== null} disabled={busy} data-testid="option-other" onClick={() => pick(OTHER)}>
            <span className="ob-tile-text">
              <span className="ob-tile-label">Other…</span>
              <span className="ob-tile-detail muted">Say it in your own words</span>
            </span>
          </button>
        )}
      </div>
      {other !== null && !answered && (
        <form
          className="ob-other-form"
          onSubmit={(e) => {
            e.preventDefault();
            submitOther();
          }}
        >
          <input className="ob-other-input" autoFocus value={other} onChange={(e) => setOther(e.target.value)} placeholder="Type your answer" aria-label="Your own answer" data-testid="option-other-input" disabled={busy} />
          <button type="submit" className="ob-send" disabled={busy || !other.trim()} data-testid="option-other-send">
            Send
          </button>
        </form>
      )}
      {board.multiSelect && !answered && (
        <div className="ob-actions">
          <button type="button" className="ob-send" disabled={busy || picked.length === 0} data-testid="option-send" onClick={submitMulti}>
            Send{picked.length > 0 ? ` (${picked.length})` : ''}
          </button>
        </div>
      )}
      {chosen !== null && (
        <div className="ob-chosen" data-testid="option-chosen">
          <IconCheck size={13} /> Chosen: {chosen}
        </div>
      )}
      {error && (
        <div className="ob-error" role="alert" data-testid="option-error">
          {error}
        </div>
      )}
    </div>
  );
}
