// The desktop pet's page: what the pet window shows instead of the app (main.tsx picks it when the
// address says `window=pet`). A creature on a transparent background, a speech bubble, dragging,
// a click that opens the chat that needs the user, and a small menu on a right click.
//
// The window can call very few ops (the host lists them), takes no focus when it appears, and is
// never zoomed. Colours, the motion level and own colours arrive through the same appearance
// wiring as the main window.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type CSSProperties, type PointerEvent as ReactPointerEvent } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { connectAppearance } from '../shell/theme.ts';
import { flair } from '../shell/motion.ts';
import { parseSounds, playCue, SOUNDS_DEFAULT } from '../shell/sounds.ts';
import { PetBrain } from './brain.ts';
import { Creature, useBlink, useMayMove } from './Creature.tsx';
import { PET_DEFAULT, PET_PX, parsePet, throttled, type Pose } from './model.ts';
import { preloadWindowApi, startDragging, watchMoves } from './windowBridge.ts';
import './pet.css';

/** A press that moves more than this many pixels is a drag, not a click. */
const DRAG_PX = 4;
/** The window reports where it was dropped at most this often. */
const MOVED_EVERY_MS = 350;
/** Poses in which the eyes blink. */
const AWAKE: Pose[] = ['idle', 'working', 'waiting', 'celebrate'];

/** A stored preference, read once and followed when the host says it changed. */
function useStored<T>(api: Api, bus: EventBus, key: string, parse: (v: unknown) => T, start: T): T {
  const [value, setValue] = useState<T>(start);
  useEffect(() => {
    let live = true;
    api<unknown>('preference_get', { key })
      .then((v) => live && setValue(parse(v)))
      .catch(() => undefined);
    const off = bus.on('preferences', (e) => {
      const changed = (e.changed ?? {}) as Record<string, unknown>;
      if (key in changed) setValue(parse(changed[key]));
    });
    return () => {
      live = false;
      off();
    };
  }, [api, bus, key, parse]);
  return value;
}

export function PetApp({ api, bus }: { api: Api; bus: EventBus }) {
  const brain = useMemo(() => new PetBrain(), []);
  const view = useSyncExternalStore(brain.subscribe, brain.get);
  const pref = useStored(api, bus, 'desktopPet', parsePet, PET_DEFAULT);
  const sounds = useStored(api, bus, 'sounds', parseSounds, SOUNDS_DEFAULT);
  const moving = useMayMove();
  const blink = useBlink(moving && AWAKE.includes(view.pose));
  const [menu, setMenu] = useState(false);
  const press = useRef<{ x: number; y: number; dragging: boolean } | null>(null);

  useEffect(() => brain.connect(api, bus), [brain, api, bus]);
  useEffect(() => {
    document.documentElement.dataset.window = 'pet';
    // The window's zoom belongs to the main window; the pet may not ask for it.
    const petApi: Api = <T,>(op: string, args?: unknown) => (op === 'window_zoom' ? Promise.resolve(null as T) : api<T>(op, args));
    const stop = connectAppearance(petApi, bus, document.documentElement);
    preloadWindowApi();
    // The host keeps the window hidden until the first frame is drawn.
    const frame = requestAnimationFrame(() => void api('pet_ready').catch(() => undefined));
    return () => {
      cancelAnimationFrame(frame);
      stop();
    };
  }, [api, bus]);
  // Where the window was dropped goes to the host, which keeps it for the next start.
  useEffect(() => {
    const report = throttled((x: number, y: number) => void api('pet_moved', { x, y }).catch(() => undefined), MOVED_EVERY_MS);
    let off = () => {};
    let gone = false;
    void watchMoves(report.call).then((o) => (gone ? o() : (off = o)));
    return () => {
      gone = true;
      off();
      report.cancel();
    };
  }, [api]);
  // The menu goes when the window loses focus or Escape is pressed.
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(false);
    const key = (e: KeyboardEvent) => e.key === 'Escape' && close();
    window.addEventListener('blur', close);
    window.addEventListener('keydown', key);
    return () => {
      window.removeEventListener('blur', close);
      window.removeEventListener('keydown', key);
    };
  }, [menu]);

  const open = (args: { chatId?: string | null; settings?: boolean }) => {
    setMenu(false);
    void api('pet_open', args).catch(() => undefined);
  };
  const click = () => {
    // The bounce is a small interface sound: it follows that switch and the quiet level.
    if (sounds.cues && flair() !== 'quiet') playCue('bounce', sounds);
    open(view.target ? { chatId: view.target } : {});
  };
  const down = (e: ReactPointerEvent) => {
    if (e.button !== 0) return;
    setMenu(false);
    press.current = { x: e.clientX, y: e.clientY, dragging: false };
  };
  const move = (e: ReactPointerEvent) => {
    const p = press.current;
    if (!p || p.dragging || Math.hypot(e.clientX - p.x, e.clientY - p.y) <= DRAG_PX) return;
    p.dragging = true;
    void startDragging();
  };
  const up = (e: ReactPointerEvent) => {
    const p = press.current;
    press.current = null;
    if (e.button === 0 && p && !p.dragging) click();
  };

  return (
    <div
      className="pet-stage"
      data-pet-motion={moving ? 'on' : 'off'}
      data-pose={view.pose}
      style={{ '--pet-size': `${PET_PX[pref.size]}px` } as CSSProperties}
      onContextMenu={(e) => {
        e.preventDefault();
        setMenu(true);
      }}
    >
      {pref.bubble && view.bubble && !menu && (
        <div className="pet-bubble" data-testid="pet-bubble">
          {view.bubble}
        </div>
      )}
      <button
        type="button"
        className="pet-creature"
        data-testid="pet-creature"
        data-pose={view.pose}
        data-kind={pref.kind}
        aria-label={view.bubble ? `Helm pet: ${view.bubble}` : 'Helm pet'}
        onPointerDown={down}
        onPointerMove={move}
        onPointerUp={up}
        onPointerCancel={() => (press.current = null)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            click();
          }
        }}
      >
        <Creature kind={pref.kind} pose={view.pose} blink={blink} />
      </button>
      {menu && (
        <div className="pet-menu" data-testid="pet-menu" role="menu">
          <button type="button" role="menuitem" onClick={() => open({})}>
            Open Helm
          </button>
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setMenu(false);
              void api('pet_hide').catch(() => undefined);
            }}
          >
            Hide pet
          </button>
          <button type="button" role="menuitem" onClick={() => open({ settings: true })}>
            Pet settings…
          </button>
        </div>
      )}
    </div>
  );
}
