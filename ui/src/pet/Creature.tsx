// One creature by kind, and the two small hooks that bring it to life: whether it may move, and
// when its eyes blink. Shared by the pet window and the settings previews.

import { useEffect, useState } from 'react';
import { mayMove, onMotionChange } from '../shell/motion.ts';
import { Crab } from './creatures/Crab.tsx';
import { Lantern } from './creatures/Lantern.tsx';
import { Puffin } from './creatures/Puffin.tsx';
import type { PetKind, Pose } from './model.ts';

export function Creature({ kind, pose, blink }: { kind: PetKind; pose: Pose; blink: boolean }) {
  if (kind === 'crab') return <Crab pose={pose} blink={blink} />;
  if (kind === 'lantern') return <Lantern pose={pose} blink={blink} />;
  return <Puffin pose={pose} blink={blink} />;
}

/** True while decoration may move; follows the quiet level, reduced motion and the slow-display mode. */
export function useMayMove(): boolean {
  const [moving, setMoving] = useState(mayMove);
  useEffect(() => {
    setMoving(mayMove());
    return onMotionChange(() => setMoving(mayMove()));
  }, []);
  return moving;
}

/** The eyes shut for 120 ms every 4 to 7 seconds while `active`. */
export function useBlink(active: boolean): boolean {
  const [shut, setShut] = useState(false);
  useEffect(() => {
    if (!active) {
      setShut(false);
      return;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    let alive = true;
    const wait = () => {
      timer = setTimeout(() => {
        setShut(true);
        timer = setTimeout(() => {
          setShut(false);
          if (alive) wait();
        }, 120);
      }, 4000 + Math.random() * 3000);
    };
    wait();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [active]);
  return shut;
}

