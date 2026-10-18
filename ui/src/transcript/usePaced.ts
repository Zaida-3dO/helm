// React glue for the pacer and the thinking preview.

import { useEffect, useRef, useState } from 'react';
import { Pacer } from './pacer.ts';
import { prefersReducedMotion, shouldPace } from './motion.ts';
import { PREVIEW_MS, thinkingPreview } from './thinking.ts';

/** The visible part of a streaming text. Rows that are not streaming show their text at once. */
export function usePacedText(text: string, streaming: boolean): string {
  const pacer = useRef<Pacer | null>(null);
  const [shown, setShown] = useState(streaming ? '' : text);

  useEffect(() => {
    // Reduce Motion: no word-by-word reveal, the text shows at once.
    if (!shouldPace(streaming, pacer.current !== null, prefersReducedMotion())) {
      pacer.current = null;
      setShown(text);
      return;
    }
    if (!pacer.current) pacer.current = new Pacer();
    const p = pacer.current;
    p.push(text, performance.now(), !streaming, document.hidden);
    setShown(p.text);
    let frame = 0;
    const loop = () => {
      const t = p.tick(performance.now());
      if (t !== null) setShown(t);
      if (!p.finished && p.text.length < text.length) frame = requestAnimationFrame(loop);
    };
    frame = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(frame);
  }, [text, streaming]);

  return shown;
}

/** The thinking preview, refreshed at most every PREVIEW_MS. */
export function useThinkingPreview(thinking: string): string {
  const [preview, setPreview] = useState(() => thinkingPreview(thinking));
  const last = useRef(0);
  const latest = useRef(thinking);
  latest.current = thinking;
  useEffect(() => {
    const wait = PREVIEW_MS - (Date.now() - last.current);
    const run = () => {
      last.current = Date.now();
      setPreview(thinkingPreview(latest.current));
    };
    if (wait <= 0) {
      run();
      return;
    }
    const t = setTimeout(run, wait);
    return () => clearTimeout(t);
  }, [thinking]);
  return preview;
}
