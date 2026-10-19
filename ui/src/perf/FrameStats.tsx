// The frame-stats overlay: Mod+Shift+P from anywhere, hidden at the start
// of every session and never saved. Click-through. It samples only while it is open, so it costs
// nothing when hidden. Below its rows it adds the newest send's timings from
// `window.__helmPerf`.

import { useEffect, useState, useSyncExternalStore } from 'react';
import { keys } from '../ui/keys.ts';
import { motion, watchCadence } from './cadence.ts';
import { FrameSampler, type FrameSample } from './frameStats.ts';
import { perf, setOverlayToggle, watchSends } from './perf.ts';
import './perf.css';

export const PERFORMANCE_BINDING = 'performance';

export function FrameStats() {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const toggle = () => setOpen((o) => !o);
    const offKey = keys.register({ id: PERFORMANCE_BINDING, title: 'Toggle FPS / frame stats', defaultKeys: 'Ctrl+Shift+P', run: toggle });
    const offToggle = setOverlayToggle(toggle);
    const offSends = watchSends(document);
    const offCadence = watchCadence(document);
    return () => {
      offKey();
      offToggle();
      offSends();
      offCadence();
    };
  }, []);
  return open ? <Overlay /> : null;
}

const ms = (v: number | null | undefined) => (v == null ? '—' : `${v.toFixed(1)} ms`);
const whole = (v: number | null | undefined) => (v == null ? '—' : `${Math.round(v)} ms`);

function useHidden(): boolean {
  return useSyncExternalStore(
    (on) => {
      document.addEventListener('visibilitychange', on);
      return () => document.removeEventListener('visibilitychange', on);
    },
    () => document.hidden,
  );
}

function Overlay() {
  const [sample, setSample] = useState<FrameSample | null>(null);
  const hidden = useHidden();
  const chord = useSyncExternalStore(keys.subscribe, () => keys.chordFor(PERFORMANCE_BINDING));

  useEffect(() => {
    if (hidden) {
      setSample(null);
      return;
    }
    const sampler = new FrameSampler();
    let raf = requestAnimationFrame(function loop(t) {
      const s = sampler.frame(t);
      if (s) setSample(s);
      raf = requestAnimationFrame(loop);
    });
    return () => cancelAnimationFrame(raf);
  }, [hidden]);

  const send = perf.last();
  const low = useSyncExternalStore(motion.subscribe, () => motion.low);
  const footer = hidden ? 'Paused while hidden' : sample ? `${sample.frames} frames / ${(sample.elapsedMs / 1000).toFixed(2)} s` : 'Sampling…';

  return (
    <div className="frame-stats" data-testid="frame-stats" aria-hidden="true">
      <div className="frame-stats-head">
        <span className="frame-stats-fps">{sample ? Math.round(sample.fps) : '—'}</span>
        <span className="frame-stats-label">FPS</span>
        {chord && <kbd className="frame-stats-key">{chord}</kbd>}
      </div>
      <dl className="frame-stats-grid">
        <dt>Average frame</dt>
        <dd>{ms(sample?.avgMs)}</dd>
        <dt>p95 frame</dt>
        <dd>{ms(sample?.p95Ms)}</dd>
        <dt>Worst frame</dt>
        <dd>{ms(sample?.worstMs)}</dd>
        <dt>Frames &gt; 50 ms</dt>
        <dd>{sample ? sample.slow : '—'}</dd>
        <dt>Send → row</dt>
        <dd>{whole(send?.rowMs)}</dd>
        <dt>Send → first word</dt>
        <dd>{whole(send?.firstWordMs)}</dd>
        <dt>Motion</dt>
        <dd title={low ? 'Low refresh rate: motion is reduced' : undefined}>{low ? 'reduced' : 'full'}</dd>
      </dl>
      <div className="frame-stats-foot">
        <div>{footer}</div>
        <div>rAF callbacks · not GPU FPS</div>
      </div>
    </div>
  );
}
