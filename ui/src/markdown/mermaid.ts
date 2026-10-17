// Mermaid, loaded on first use so it stays out of the main bundle (it is the largest dependency).
//
// Styles: Mermaid writes a <style> element inside each SVG it returns and sets some inline style
// attributes. Both are allowed by the app's `style-src 'self' 'unsafe-inline'`; no
// script is evaluated. `securityLevel: 'strict'` makes Mermaid sanitise labels itself.

type MermaidApi = typeof import('mermaid').default;

let loading: Promise<MermaidApi> | null = null;
let counter = 0;
const svgCache = new Map<string, string>();
const SVG_CACHE_ENTRIES = 50;

function prefersDark(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches;
}

function load(): Promise<MermaidApi> {
  loading ??= import('mermaid').then((m) => {
    m.default.initialize({
      startOnLoad: false,
      securityLevel: 'strict',
      theme: prefersDark() ? 'dark' : 'default',
    });
    return m.default;
  });
  return loading;
}

/** Renders diagram source to an SVG string. Rejects with Mermaid's message on a syntax error. */
export async function renderDiagram(source: string): Promise<string> {
  const dark = prefersDark();
  const key = `${dark ? 'd' : 'l'}\u0000${source}`;
  const hit = svgCache.get(key);
  if (hit !== undefined) return hit;

  const mermaid = await load();
  const id = `helm-mmd-${++counter}`;
  try {
    const { svg } = await mermaid.render(id, source);
    svgCache.set(key, svg);
    if (svgCache.size > SVG_CACHE_ENTRIES) {
      const oldest = svgCache.keys().next().value;
      if (oldest !== undefined) svgCache.delete(oldest);
    }
    return svg;
  } catch (error) {
    // Mermaid leaves a scratch element behind when it fails.
    document.getElementById(`d${id}`)?.remove();
    throw error;
  }
}
