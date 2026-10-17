// Web Worker entry: renders Markdown off the main thread. Pure: it posts HTML strings back and
// touches nothing else (no `api`, no network).
import type { RenderRequest, RenderResponse } from './offload.ts';
import { renderToHtml } from './render.ts';

interface WorkerScope {
  onmessage: ((event: { data: RenderRequest }) => void) | null;
  postMessage(message: RenderResponse): void;
}

const scope = self as unknown as WorkerScope;

scope.onmessage = (event) => {
  const { id, text, options } = event.data;
  scope.postMessage({ id, html: renderToHtml(text, options) });
};
