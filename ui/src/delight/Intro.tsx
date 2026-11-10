// The first-run intro: the wheel draws itself, turns a quarter and settles, "Helm" rises and the
// line "Your crew is ready." follows, 2.8 s in all. Shown once per profile on this device. A click
// or any key skips it. When motion is off a still card fades in and out instead. It is never shown
// in a test run in the background window, or in a window that is not on screen yet.

import { useEffect, useState } from 'react';
import type { Api } from '../api/client.ts';
import { colourProfileKey } from '../shell/colours.ts';
import { mayMove } from '../shell/motion.ts';
import { playCue } from '../shell/sounds.ts';
import { WheelMark, Wordmark } from '../ui/status/index.ts';
import { WheelOutline } from '../ui/status/WheelOutline.tsx';
import { INTRO_MS, INTRO_SETTLE_MS, INTRO_STILL_MS, introKey, introSeen, markIntroSeen, whenVisible } from './intro.ts';
import './intro.css';

type Phase = 'off' | 'motion' | 'still';

export function Intro({ api }: { api: Api }) {
  const [phase, setPhase] = useState<Phase>('off');

  useEffect(() => {
    let live = true;
    let cancel = () => {};
    void (async () => {
      const profile = colourProfileKey(await api<unknown>('preference_get', { key: 'activeProfile' }).catch(() => null));
      const key = introKey(profile);
      const store = typeof localStorage === 'undefined' ? null : localStorage;
      if (introSeen(store, key)) return;
      // The host says no for a test run in the background window; an answer it cannot give counts as no.
      const allowed = await api<{ allowed?: boolean }>('intro_allowed')
        .then((r) => r?.allowed === true)
        .catch(() => false);
      if (!allowed || !live) return;
      cancel = whenVisible(document, () => {
        markIntroSeen(store, key);
        setPhase(mayMove() ? 'motion' : 'still');
      });
    })();
    return () => {
      live = false;
      cancel();
    };
  }, [api]);

  useEffect(() => {
    if (phase === 'off') return;
    const moving = phase === 'motion';
    const done = () => setPhase('off');
    const end = setTimeout(done, moving ? INTRO_MS : INTRO_STILL_MS);
    // One chime as the wheel settles (silent unless sounds are on, and always under Zen).
    const chime = setTimeout(() => playCue('complete'), moving ? INTRO_SETTLE_MS : 400);
    window.addEventListener('keydown', done, true);
    return () => {
      clearTimeout(end);
      clearTimeout(chime);
      window.removeEventListener('keydown', done, true);
    };
  }, [phase]);

  if (phase === 'off') return null;
  return (
    <div className="delight-intro" data-testid="intro" data-phase={phase} role="presentation" onPointerDown={() => setPhase('off')}>
      <div className="intro-stack">
        <div className="intro-wheel">
          {phase === 'motion' && <WheelOutline size={160} />}
          <WheelMark size={160} className="intro-mark" />
        </div>
        <Wordmark className="intro-title" />
        <div className="intro-tag">Your crew is ready.</div>
      </div>
    </div>
  );
}
