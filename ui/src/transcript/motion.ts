// Reduce Motion for JS-driven effects (effects-and-motion: with Reduce Motion on, streaming text
// shows at once instead of being revealed word by word). CSS animations are handled in CSS.

/** Whether the system asks for reduced motion. False where matchMedia is missing (tests, SSR). */
export function prefersReducedMotion(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-reduced-motion: reduce)').matches;
}

/** Whether a streaming text should be paced, or shown at once. */
export function shouldPace(streaming: boolean, pacing: boolean, reducedMotion: boolean): boolean {
  if (reducedMotion) return false;
  return streaming || pacing;
}
