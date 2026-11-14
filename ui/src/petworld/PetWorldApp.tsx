// The roaming pet's page (a prototype; the host opens it only in debug builds with its switch set).
// main.tsx picks it when the address says `window=petworld`. The window is exactly the sprite and
// the host moves it: this page only draws the creature, in the mood's pose and the body's state
// (walking, held, falling, landing), and reports the mood and presses. A press that moves more
// than a few pixels grabs the pet, and the host follows the cursor until it is let go; a press
// that does not is a click, which opens the chat that needs the user.

import { useEffect, useMemo, useRef, useState, useSyncExternalStore, type PointerEvent as ReactPointerEvent } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { connectAppearance } from '../shell/theme.ts';
import { PetBrain } from '../pet/brain.ts';
import { Creature, useBlink, useMayMove } from '../pet/Creature.tsx';
import { PET_DEFAULT, parsePet, type PetKind, type Pose } from '../pet/model.ts';
import '../pet/pet.css';
import './petworld.css';

/** A press that moves more than this many pixels is a grab, not a click. */
const GRAB_PX = 4;
/** Poses in which the eyes blink. */
const AWAKE: Pose[] = ['idle', 'working', 'waiting', 'celebrate'];

type BodyState = 'idle' | 'walk' | 'think' | 'fall' | 'land' | 'held';
const BODY_STATES: readonly BodyState[] = ['idle', 'walk', 'think', 'fall', 'land', 'held'];

export function PetWorldApp({ api, bus }: { api: Api; bus: EventBus }) {
  const brain = useMemo(() => new PetBrain(), []);
  const view = useSyncExternalStore(brain.subscribe, brain.get);
  const moving = useMayMove();
  const blink = useBlink(moving && AWAKE.includes(view.pose));
  const [kind, setKind] = useState<PetKind>(PET_DEFAULT.kind);
  const [body, setBody] = useState<{ state: BodyState; facing: 'left' | 'right' }>({ state: 'fall', facing: 'right' });
  const press = useRef<{ x: number; y: number; grabbed: boolean } | null>(null);

  useEffect(() => brain.connect(api, bus), [brain, api, bus]);
  useEffect(() => {
    document.documentElement.dataset.window = 'petworld';
    // The window's zoom belongs to the main window; the pet may not ask for it.
    const petApi: Api = <T,>(op: string, args?: unknown) => (op === 'window_zoom' ? Promise.resolve(null as T) : api<T>(op, args));
    const stop = connectAppearance(petApi, bus, document.documentElement);
    // The host keeps the window hidden until the first frame is drawn.
    const frame = requestAnimationFrame(() => void api('petworld_ready').catch(() => undefined));
    return () => {
      cancelAnimationFrame(frame);
      stop();
    };
  }, [api, bus]);
  // The creature is the ordinary pet's kind.
  useEffect(() => {
    let live = true;
    api<unknown>('preference_get', { key: 'desktopPet' })
      .then((v) => live && setKind(parsePet(v).kind))
      .catch(() => undefined);
    const off = bus.on('preferences', (e) => {
      const changed = (e.changed ?? {}) as Record<string, unknown>;
      if ('desktopPet' in changed) setKind(parsePet(changed.desktopPet).kind);
    });
    return () => {
      live = false;
      off();
    };
  }, [api, bus]);
  useEffect(
    () =>
      bus.on('petworld_body', (e) => {
        const state = BODY_STATES.find((s) => s === e.state);
        if (state) setBody({ state, facing: e.facing === 'left' ? 'left' : 'right' });
      }),
    [bus],
  );
  // The mood steers wandering: no walks while a chat works or waits, none at all with motion off.
  useEffect(() => {
    void api('petworld_mood', { pose: view.pose, still: !moving }).catch(() => undefined);
  }, [api, view.pose, moving]);

  const letGo = () => {
    const p = press.current;
    press.current = null;
    if (p?.grabbed) void api('petworld_release').catch(() => undefined);
    return p;
  };
  const down = (e: ReactPointerEvent<HTMLButtonElement>) => {
    if (e.button !== 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    press.current = { x: e.clientX, y: e.clientY, grabbed: false };
  };
  const move = (e: ReactPointerEvent) => {
    const p = press.current;
    if (!p || p.grabbed || Math.hypot(e.clientX - p.x, e.clientY - p.y) <= GRAB_PX) return;
    p.grabbed = true;
    void api('petworld_grab', { x: p.x, y: p.y }).catch(() => undefined);
  };
  const up = (e: ReactPointerEvent) => {
    const p = letGo();
    if (e.button === 0 && p && !p.grabbed) void api('pet_open', view.target ? { chatId: view.target } : {}).catch(() => undefined);
  };

  return (
    <div className="pet-preview petworld-stage" data-pet-motion={moving ? 'on' : 'off'} data-body={body.state} data-facing={body.facing} onContextMenu={(e) => e.preventDefault()}>
      <button
        type="button"
        className="petworld-creature"
        data-testid="petworld-creature"
        aria-label={view.bubble ? `Helm pet: ${view.bubble}` : 'Helm pet'}
        onPointerDown={down}
        onPointerMove={move}
        onPointerUp={up}
        onPointerCancel={() => void letGo()}
        onLostPointerCapture={() => void letGo()}
      >
        <span className="petworld-body">
          <Creature kind={kind} pose={view.pose} blink={blink} />
        </span>
      </button>
    </div>
  );
}
