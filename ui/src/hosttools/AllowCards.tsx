// Allow cards in the composer dock.
//
// - The list op is the only source of cards: a `consent` event, window focus and every decide
//   trigger a refetch, and nothing is shown optimistically.
// - Allow and Deny count only after the settle gate (400 ms with the button's slot still within
//   0.5 px), from trusted events outside dev builds. Neither button is default or focused, there
//   is no shortcut, and the cards are not a form.
// - The agent's reason and evidence render inert, marked as the agent's words.

import { useCallback, useEffect, useMemo, useRef, useState, type MouseEvent } from 'react';
import { consentOps, type ConsentRequest } from '../api/consent.ts';
import { useShell } from '../shell/context.ts';
import { inertLines } from './inert.ts';
import { SettleGate, type Rect } from './settle.ts';
import './hosttools.css';

function rectOf(el: Element): Rect {
  const b = el.getBoundingClientRect();
  return { x: b.x, y: b.y, width: b.width, height: b.height };
}

function message(e: unknown): string {
  return String((e as { message?: unknown })?.message ?? e);
}

function Inert({ text, label }: { text: string; label: string }) {
  if (!text.trim()) return null;
  return (
    <div className="allow-agent-text">
      <div className="allow-agent-label">{label}</div>
      {inertLines(text).map((line, i) => (
        <div key={i} className="allow-agent-line" dir="auto">
          {line}
        </div>
      ))}
    </div>
  );
}

export function AllowCards({ chatId }: { chatId: string }) {
  const shell = useShell();
  const ops = useMemo(() => consentOps(shell.api), [shell.api]);
  const [cards, setCards] = useState<ConsentRequest[]>([]);
  const [minutes, setMinutes] = useState<Record<string, number>>({});
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const box = useRef<HTMLDivElement | null>(null);
  const slots = useRef(new Map<string, HTMLElement>());
  const gate = useRef(new SettleGate(performance.now()));
  // One decide in flight at a time: a double-click sends one request, not a refused second.
  const inFlight = useRef(false);

  const refetch = useCallback(() => {
    ops
      .list(chatId)
      .then((l) => setCards(l.pending))
      .catch((e: unknown) => setError(message(e)));
  }, [ops, chatId]);

  useEffect(() => {
    refetch();
    const off = shell.bus.on('consent', (e) => {
      if ((e as { chatId?: unknown }).chatId === chatId) refetch();
    });
    window.addEventListener('focus', refetch);
    return () => {
      off();
      window.removeEventListener('focus', refetch);
    };
  }, [shell.bus, chatId, refetch]);

  // The card set changed: start the settle window over, and keep focus from being lost.
  const ids = cards.map((c) => c.id).join(',');
  useEffect(() => {
    gate.current.reset(performance.now());
    const active = document.activeElement;
    if (box.current && (!active || active === document.body)) box.current.focus({ preventScroll: true });
  }, [ids]);

  // Sample every button slot each frame; anything that can move the cards resets the window.
  useEffect(() => {
    if (cards.length === 0) return;
    let raf = 0;
    const tick = () => {
      const rects = new Map<string, Rect>();
      for (const [id, el] of slots.current) rects.set(id, rectOf(el));
      gate.current.frame(performance.now(), rects);
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    const reset = () => gate.current.reset(performance.now());
    const onVisible = () => {
      if (document.visibilityState === 'visible') reset();
    };
    const ro = new ResizeObserver(reset);
    const el = box.current;
    if (el) ro.observe(el);
    window.addEventListener('resize', reset);
    document.addEventListener('visibilitychange', onVisible);
    el?.addEventListener('scroll', reset, true);
    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      window.removeEventListener('resize', reset);
      document.removeEventListener('visibilitychange', onVisible);
      el?.removeEventListener('scroll', reset, true);
    };
  }, [cards.length]);

  if (cards.length === 0) return null;

  const decide = (c: ConsentRequest, decision: 'allow' | 'deny', slotId: string) => (e: MouseEvent) => {
    if (!e.isTrusted && !import.meta.env.DEV) return;
    if (inFlight.current) return;
    const slot = slots.current.get(slotId);
    if (!slot || !gate.current.canDecide(slotId, performance.now(), rectOf(slot))) return;
    inFlight.current = true;
    const grantMinutes = decision === 'allow' ? (minutes[c.id] ?? c.grantMinutes) : null;
    const note = notes[c.id]?.trim() || null;
    ops
      .decide({ requestId: c.id, decision, grantMinutes, note })
      .then(() => setError(null))
      .catch((err: unknown) => setError(message(err)))
      .finally(() => {
        inFlight.current = false;
        refetch();
      });
  };

  const slotRef = (id: string) => (el: HTMLElement | null) => {
    if (el) slots.current.set(id, el);
    else slots.current.delete(id);
  };

  return (
    <div className="allow-cards" ref={box} tabIndex={-1} role="group" aria-label="Allow requests" data-testid="allow-cards">
      {error && <div className="allow-error">{error}</div>}
      {cards.map((c) => (
        <div key={c.id} className="allow-card" data-testid="allow-card" data-request={c.id}>
          <div className="allow-title">
            <strong>{c.label}</strong>
            {/* Host-built, but it names agent- or repository-supplied targets: inert too. */}
            <span className="allow-agent-line" dir="auto">
              {inertLines(c.summary).join(' ')}
            </span>
          </div>
          <Inert label="The agent's reason (its words; Helm has not checked them)" text={c.reason} />
          <Inert label="The agent's evidence (its words; Helm has not checked them)" text={c.evidence} />
          <div className="allow-controls">
            <label>
              Grant lasts
              <input
                type="number"
                min={5}
                max={240}
                value={minutes[c.id] ?? c.grantMinutes}
                onChange={(e) => setMinutes({ ...minutes, [c.id]: Math.max(5, Math.min(240, Number(e.target.value) || c.grantMinutes)) })}
              />
              minutes
            </label>
            <input
              className="allow-note"
              type="text"
              maxLength={500}
              placeholder="Note for the agent (sent with Deny)"
              value={notes[c.id] ?? ''}
              onChange={(e) => setNotes({ ...notes, [c.id]: e.target.value })}
            />
            <span className="allow-slot" ref={slotRef(`${c.id}:deny`)}>
              <button type="button" className="ui-btn" data-testid="allow-deny" onClick={decide(c, 'deny', `${c.id}:deny`)}>
                Deny
              </button>
            </span>
            <span className="allow-slot" ref={slotRef(`${c.id}:allow`)}>
              <button type="button" className="ui-btn" data-testid="allow-allow" onClick={decide(c, 'allow', `${c.id}:allow`)}>
                Allow
              </button>
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}
