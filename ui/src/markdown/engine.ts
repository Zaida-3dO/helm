// The browser-side engine: one shared offloader (and so one cache) for every Markdown consumer.
// Kept apart from offload.ts because `new Worker(new URL(...))` only exists in the bundled app.
import { RenderOffloader } from './offload.ts';
import type { WorkerLike } from './offload.ts';
import { renderToHtml } from './render.ts';
import type { RenderOptions } from './render.ts';

/** Text longer than this is parsed in the worker; shorter text is cheaper to render in place. */
export const LONG_TEXT_CHARS = 4000;

let shared: RenderOffloader | null = null;

export function sharedOffloader(): RenderOffloader {
  shared ??= new RenderOffloader({
    createWorker: () =>
      new Worker(new URL('./markdown.worker.ts', import.meta.url), {
        type: 'module',
      }) as unknown as WorkerLike,
  });
  return shared;
}

/** Synchronous render for short text, through the shared cache. */
export function renderShort(text: string, options: RenderOptions): string {
  const off = sharedOffloader();
  const hit = off.cached(text, options);
  if (hit !== undefined) return hit;
  const html = renderToHtml(text, options);
  off.remember(text, options, html);
  return html;
}
