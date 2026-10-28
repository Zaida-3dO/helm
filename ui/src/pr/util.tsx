// Small shared pieces of the PR view: people as initials, times, pills, the typed-text box.

import { useRef, useSyncExternalStore } from 'react';
import type { ReactNode } from 'react';
import { Button } from '../ui/index.ts';
import { attempt, clickFrom } from './client.ts';
import type { Click, WriteOutcome } from './client.ts';
import { openOnGitHub } from './open.ts';
import type { PrEntry } from './store.ts';
import type { PrState } from './types.ts';

export function useEntry(entry: PrEntry): void {
  useSyncExternalStore(entry.subscribe, entry.getVersion);
}

// ---- time ----

const MIN = 60_000;

/** "just now", "5 minutes ago", "3 hours ago", "2 days ago", then a date. */
export function relTime(iso: string, now = Date.now()): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return '';
  const d = Math.max(0, now - t);
  if (d < MIN) return 'just now';
  if (d < 60 * MIN) return plural(Math.floor(d / MIN), 'minute');
  if (d < 24 * 60 * MIN) return plural(Math.floor(d / (60 * MIN)), 'hour');
  if (d < 30 * 24 * 60 * MIN) return plural(Math.floor(d / (24 * 60 * MIN)), 'day');
  return `on ${new Date(t).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })}`;
}

const plural = (n: number, unit: string) => `${n} ${unit}${n === 1 ? '' : 's'} ago`;

export function fullDate(iso: string): string {
  const t = Date.parse(iso);
  return Number.isFinite(t) ? new Date(t).toLocaleString() : '';
}

/** The refresh note: "just now" under 5 s, then seconds, minutes, hours. */
export function ageNote(at: number, now = Date.now()): string {
  if (!Number.isFinite(at)) return '';
  const s = Math.max(0, Math.floor((now - at) / 1000));
  if (s < 5) return 'just now';
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return relTime(new Date(at).toISOString(), now);
}

// ---- people ----

export function initials(login: string): string {
  const parts = login.split(/[^A-Za-z0-9]+/).filter(Boolean);
  const text = parts.length > 1 ? `${parts[0]![0]}${parts[1]![0]}` : (parts[0] ?? '?').slice(0, 2);
  return text.toUpperCase();
}

/** One of eight fixed tones, from the login alone. */
export function tone(login: string): number {
  let h = 0;
  for (const c of login) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 8;
}

export function Avatar({ login, size = 24 }: { login: string; size?: number }) {
  return (
    <span className={`pr-avatar pr-tone-${tone(login)}`} style={{ width: size, height: size, fontSize: Math.round(size * 0.4) }} aria-hidden="true" title={login}>
      {initials(login)}
    </span>
  );
}

const ASSOCIATION: Record<string, string> = { OWNER: 'Owner', MEMBER: 'Member', COLLABORATOR: 'Collaborator', CONTRIBUTOR: 'Contributor', FIRST_TIME_CONTRIBUTOR: 'First-time contributor' };

export function AssociationBadge({ value }: { value: string }) {
  const text = ASSOCIATION[value];
  return text ? <span className="pr-badge">{text}</span> : null;
}

// ---- pills ----

export function StatePill({ state, draft }: { state: PrState; draft: boolean }) {
  const kind = state === 'MERGED' ? 'merged' : state === 'CLOSED' ? 'closed' : draft ? 'draft' : 'open';
  const text = { merged: 'Merged', closed: 'Closed', draft: 'Draft', open: 'Open' }[kind];
  return (
    <span className={`pr-pill pr-pill-${kind}`} data-testid="pr-state">
      {text}
    </span>
  );
}

// ---- the typed-text box ----

const MAX_BODY_BYTES = 60_000;
const COUNTER_FROM = 54_000;

export const byteLength = (s: string): number => new TextEncoder().encode(s).length;

export interface BoxAction {
  label: string;
  busyLabel?: string;
  primary?: boolean;
  /** Runs one write. Called with the typed text. */
  run: (click: Click, text: string) => Promise<unknown>;
  /** Called after a successful write, once the box is cleared. */
  done?: () => void;
  /** Called after an uncertain write (a check-up read). */
  uncertain?: () => void;
  /** Keeps the text after a success (not used by default). */
  allowBlank?: boolean | (() => boolean);
}

/**
 * A text box whose text lives in the PR's cache entry, so a view switch or a tab switch loses
 * nothing. Each button sends one write from its own click (or Ctrl+Enter), never twice, and an
 * uncertain outcome keeps the text.
 */
export function WriteBox({
  entry,
  id,
  label,
  placeholder,
  actions,
  onCancel,
  note,
  rows = 4,
  autoFocus,
}: {
  entry: PrEntry;
  id: string;
  label: string;
  placeholder?: string;
  actions: BoxAction[];
  onCancel?: () => void;
  note?: ReactNode;
  rows?: number;
  autoFocus?: boolean;
}) {
  useEntry(entry);
  const text = entry.drafts.get(id) ?? '';
  const sending = entry.sending.has(id);
  const outcome = entry.outcomes.get(id);
  const bytes = byteLength(text);
  const tooBig = bytes > MAX_BODY_BYTES;
  const ref = useRef<HTMLTextAreaElement>(null);

  const send = (a: BoxAction, click: Click | null) => {
    if (!click || entry.sending.has(id)) return;
    const body = entry.drafts.get(id) ?? '';
    const blankOk = typeof a.allowBlank === 'function' ? a.allowBlank() : a.allowBlank;
    if (!blankOk && !body.trim()) {
      entry.outcomes.set(id, { kind: 'refused', text: 'Write something first.' });
      entry.changed();
      return;
    }
    if (byteLength(body) > MAX_BODY_BYTES) return;
    entry.sending.add(id);
    entry.outcomes.delete(id);
    entry.changed();
    void entry
      .enqueue(() => attempt(() => a.run(click, body)))
      .then((r: WriteOutcome<unknown>) => {
        entry.sending.delete(id);
        if (r.ok) {
          // Clear only if the user has not typed something new while it was sending.
          if ((entry.drafts.get(id) ?? '') === body) entry.drafts.delete(id);
          entry.outcomes.set(id, { kind: 'ok', text: 'Sent.' });
          a.done?.();
        } else {
          entry.outcomes.set(id, { kind: r.kind, text: r.message, cls: r.cls });
          if (r.kind === 'uncertain') a.uncertain?.();
        }
        entry.changed();
      });
  };

  const primary = actions.find((a) => a.primary) ?? actions[actions.length - 1];
  return (
    <div className="pr-box" data-testid={`pr-box-${id}`}>
      <textarea
        ref={ref}
        className="pr-box-text"
        aria-label={label}
        placeholder={placeholder ?? 'Leave a comment'}
        rows={rows}
        value={text}
        disabled={sending}
        autoFocus={autoFocus}
        onChange={(e) => {
          entry.drafts.set(id, e.target.value);
          entry.changed();
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && primary) {
            e.preventDefault();
            send(primary, clickFrom(e.nativeEvent));
          } else if (e.key === 'Escape' && onCancel) {
            e.stopPropagation();
            onCancel();
          }
        }}
      />
      {note && <div className="pr-fine">{note}</div>}
      {bytes >= COUNTER_FROM && (
        <div className={tooBig ? 'pr-fine pr-bad' : 'pr-fine'}>
          {bytes.toLocaleString()} of {MAX_BODY_BYTES.toLocaleString()} bytes{tooBig ? ': too long to send' : ''}
        </div>
      )}
      {outcome && outcome.kind !== 'ok' && (
        <div className={outcome.kind === 'uncertain' ? 'pr-fine pr-warn' : 'pr-fine pr-bad'} role="alert" data-testid={`pr-box-${id}-outcome`}>
          {outcome.text}{' '}
          {outcome.cls === 'ReviewPending' && (
            <button type="button" className="pr-link-btn" onClick={() => openOnGitHub(entry.ref, entry.shown.view)}>
              Open on GitHub
            </button>
          )}
        </div>
      )}
      <div className="pr-box-actions">
        {onCancel && (
          <Button variant="secondary" onClick={onCancel} disabled={sending}>
            Cancel
          </Button>
        )}
        {actions.map((a) => (
          <Button
            key={a.label}
            variant={a.primary ? 'primary' : 'secondary'}
            busy={sending}
            disabled={tooBig}
            data-testid={`pr-box-${id}-${a.label.toLowerCase().replace(/[^a-z]+/g, '-')}`}
            onClick={(e) => send(a, clickFrom(e.nativeEvent))}
          >
            {sending ? (a.busyLabel ?? 'Sending…') : a.label}
          </Button>
        ))}
      </div>
    </div>
  );
}

export function ErrorBlock({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div className="pr-error" role="alert" data-testid="pr-error">
      <div>{message}</div>
      <Button variant="secondary" onClick={onRetry}>
        Retry
      </Button>
    </div>
  );
}
