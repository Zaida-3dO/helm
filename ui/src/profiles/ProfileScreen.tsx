// The profile screen: a full-window "Who's using Helm?" with one tile per profile, the active one
// ringed. Arrow keys move between tiles, Enter chooses, Escape or a click outside closes. The last
// tile adds a profile; "Manage profiles" opens Settings → Profiles.

import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { createPortal } from 'react-dom';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { Button, TextInput, useLayer } from '../ui/index.ts';
import { IconPlus } from '../ui/icons.tsx';
import { ProfileDisc } from './Disc.tsx';
import { useProfiles } from './hooks.ts';
import { stepTile, tileCounts } from './model.ts';
import { errorText } from './store.ts';

export interface ProfileScreenProps {
  api: Api;
  bus: EventBus;
  onClose: () => void;
  /** Open Settings → Profiles. */
  onManage: () => void;
}

export function ProfileScreen({ api, bus, onClose, onManage }: ProfileScreenProps) {
  const { store, snap } = useProfiles(api, bus);
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const grid = useRef<HTMLDivElement>(null);
  const view = snap.view;

  useLayer('dialog', true, () => {
    if (adding) {
      setAdding(false);
      setName('');
      return;
    }
    onClose();
  });
  // Focus lands on the active profile's tile, so Enter and the arrows work straight away.
  useEffect(() => {
    const tiles = grid.current?.querySelectorAll<HTMLElement>('[data-tile]');
    const active = grid.current?.querySelector<HTMLElement>('[aria-current="true"]');
    (active ?? tiles?.[0])?.focus();
  }, [view?.activeId, view?.profiles.length]);

  const choose = (id: string) => {
    if (busy) return;
    setBusy(true);
    store.ops
      .setActive(id)
      .then((v) => {
        store.apply(v);
        onClose();
      })
      .catch((e: unknown) => setError(errorText(e)))
      .finally(() => setBusy(false));
  };

  const add = (e: FormEvent) => {
    e.preventDefault();
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    store.ops
      .create({ name: n })
      .then((v) => {
        store.apply(v);
        setName('');
        setAdding(false);
        setError(null);
      })
      .catch((err: unknown) => setError(errorText(err)))
      .finally(() => setBusy(false));
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    const dir = e.key === 'ArrowRight' || e.key === 'ArrowDown' ? 1 : e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? -1 : 0;
    if (dir === 0 || (e.target as HTMLElement).tagName === 'INPUT') return;
    const tiles = [...(grid.current?.querySelectorAll<HTMLElement>('[data-tile]') ?? [])];
    const at = tiles.indexOf(document.activeElement as HTMLElement);
    const to = stepTile(tiles.length, at, dir);
    if (to < 0) return;
    e.preventDefault();
    tiles[to]!.focus();
  };

  if (!view) return null;
  const ordered = [...view.profiles].sort((a, b) => Number(b.isHome) - Number(a.isHome));
  return createPortal(
    <div
      className="pf-screen"
      role="dialog"
      aria-modal="true"
      aria-label="Choose a profile"
      data-testid="profile-screen"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <h2 className="pf-screen-title">Who&rsquo;s using Helm?</h2>
      <div ref={grid} className="pf-screen-grid" onKeyDown={onKeyDown}>
        {ordered.map((p) => (
          <button
            key={p.id}
            type="button"
            className="pf-tile"
            data-tile=""
            data-testid={`profile-tile-${p.id}`}
            aria-current={view.activeId === p.id ? 'true' : undefined}
            aria-label={`${p.name}, ${tileCounts(p)}`}
            onClick={() => choose(p.id)}
          >
            <ProfileDisc name={p.name} colour={p.colour} size={72} />
            <span className="pf-tile-name">{p.name}</span>
            <span className="pf-tile-sub">
              {tileCounts(p)}
              {p.attention > 0 && <span className="pf-tile-attention" role="img" aria-label="Something needs you" title="Something needs you" />}
            </span>
          </button>
        ))}
        {adding ? (
          <form className="pf-tile pf-tile-form" onSubmit={add}>
            <TextInput
              autoFocus
              value={name}
              maxLength={40}
              placeholder="Name"
              autoComplete="off"
              aria-label="New profile name"
              readOnly={busy}
              onChange={(e) => setName(e.target.value)}
              data-testid="profile-new-name"
            />
            <Button type="submit" variant="secondary" disabled={busy || !name.trim()}>
              Add
            </Button>
          </form>
        ) : (
          <button type="button" className="pf-tile pf-tile-add" data-tile="" data-testid="profile-tile-add" onClick={() => setAdding(true)}>
            <span className="pf-tile-plus">
              <IconPlus size={28} />
            </span>
            <span className="pf-tile-name">Add profile</span>
          </button>
        )}
      </div>
      {error && (
        <p className="pf-screen-error" role="alert">
          {error}
        </p>
      )}
      <div className="pf-screen-foot">
        <Button variant="text" onClick={onManage} data-testid="profile-screen-manage">
          Manage profiles
        </Button>
      </div>
    </div>,
    document.body,
  );
}
