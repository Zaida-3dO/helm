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

/** The app's colour mode: its own setting first, the system's when the app has not set one. */
function prefersDark(): boolean {
  if (typeof document !== 'undefined') {
    const theme = document.documentElement.dataset.theme;
    if (theme === 'dark' || theme === 'light') return theme === 'dark';
  }
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches;
}

/**
 * Neutral boxes in the interface font, instead of the library's lavender. Mermaid does colour
 * arithmetic on these, so they are plain hex values rather than the app's CSS variables.
 */
export function diagramTheme(dark: boolean): Record<string, string> {
  const font = '"Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif';
  return dark
    ? {
        fontFamily: font,
        fontSize: '13px',
        primaryColor: '#2b2b2b',
        primaryBorderColor: '#4d4d4d',
        primaryTextColor: '#e6e6e6',
        secondaryColor: '#242424',
        tertiaryColor: '#1f1f1f',
        lineColor: '#9a9a9a',
        textColor: '#e6e6e6',
        edgeLabelBackground: '#1f1f1f',
        noteBkgColor: '#2b2b2b',
        noteTextColor: '#e6e6e6',
      }
    : {
        fontFamily: font,
        fontSize: '13px',
        primaryColor: '#f1f1ef',
        primaryBorderColor: '#c8c8c4',
        primaryTextColor: '#1f1f1d',
        secondaryColor: '#e9e9e6',
        tertiaryColor: '#fafaf8',
        lineColor: '#77776f',
        textColor: '#1f1f1d',
        edgeLabelBackground: '#fafaf8',
        noteBkgColor: '#f1f1ef',
        noteTextColor: '#1f1f1d',
      };
}

const KINDS: Record<string, string> = {
  flowchart: 'Flowchart',
  graph: 'Flowchart',
  sequencediagram: 'Sequence diagram',
  classdiagram: 'Class diagram',
  statediagram: 'State diagram',
  'statediagram-v2': 'State diagram',
  erdiagram: 'ER diagram',
  gantt: 'Gantt chart',
  pie: 'Pie chart',
  journey: 'User journey',
  mindmap: 'Mind map',
  timeline: 'Timeline',
  gitgraph: 'Git graph',
};

/** What a diagram is called in its header, read from the first word of its source. */
export function diagramKind(source: string): string {
  for (const line of source.split(String.fromCharCode(10))) {
    const t = line.trim();
    if (!t || t.startsWith('%%')) continue;
    return KINDS[t.split(/\s+/)[0]!.toLowerCase()] ?? 'Diagram';
  }
  return 'Diagram';
}

let configuredFor: boolean | null = null;

type KatexApi = typeof import('katex').default;
let katex: Promise<KatexApi> | null = null;

/**
 * Turns TeX into MathML (the webview draws it with the system's math font, so there are no
 * fonts or stylesheet to ship). Loaded on first use. Rejects on a TeX error.
 */
export async function renderMath(tex: string, display: boolean): Promise<string> {
  katex ??= import('katex').then((m) => m.default);
  const k = await katex;
  return k.renderToString(tex, { output: 'mathml', displayMode: display, throwOnError: true, strict: 'ignore', trust: false, maxSize: 20, maxExpand: 1000 });
}

async function load(dark: boolean): Promise<MermaidApi> {
  loading ??= import('mermaid').then((m) => m.default);
  const mermaid = await loading;
  if (configuredFor !== dark) {
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: 'base', themeVariables: diagramTheme(dark) });
    configuredFor = dark;
  }
  return mermaid;
}

/** Renders diagram source to an SVG string. Rejects with Mermaid's message on a syntax error. */
export async function renderDiagram(source: string): Promise<string> {
  const dark = prefersDark();
  const key = `${dark ? 'd' : 'l'}\u0000${source}`;
  const hit = svgCache.get(key);
  if (hit !== undefined) return hit;

  const mermaid = await load(dark);
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
