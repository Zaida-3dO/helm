import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { MouseEvent } from 'react';
import { LONG_TEXT_CHARS, renderShort, sharedOffloader } from './engine.ts';
import { targetFromAttrs } from './links.ts';
import type { LinkTarget } from './links.ts';
import { diagramKind, renderDiagram } from './mermaid.ts';
import { StreamBlocks } from './blocks.ts';
import { renderPlain, renderToHtml } from './render.ts';
import type { RenderOptions } from './render.ts';
import './markdown.css';

export interface MarkdownProps {
  text: string;
  /** The last block may be unfinished; Mermaid renders only once its fence is closed. */
  streaming?: boolean;
  /** Enables Mermaid diagrams. Off for previews. */
  interactive?: boolean;
  /** A vault or url link was clicked. Without a handler, clicks do nothing (they never navigate). */
  onLink?: (target: LinkTarget) => void;
  /**
   * Content for `![[...]]` embeds: Markdown text for a note (the section, if the target names a
   * heading or block), or a `data:image/...` / `blob:` URL for an image. Null leaves the card as a
   * link. Embedded notes render one level deep.
   */
  resolveEmbed?: (target: LinkTarget) => Promise<string | null>;
  className?: string;
}

function useRenderedHtml(text: string, options: RenderOptions): string {
  const { streaming, interactive, nested } = options;
  const stable = useMemo<RenderOptions>(
    () => ({ streaming, interactive, nested }),
    [streaming, interactive, nested],
  );
  const long = text.length > LONG_TEXT_CHARS;
  const off = sharedOffloader();

  const [workerHtml, setWorkerHtml] = useState<string | null>(null);
  useEffect(() => {
    if (!long) return;
    let cancelled = false;
    off
      .render(text, stable)
      .then((html) => {
        if (!cancelled) setWorkerHtml(html);
      })
      .catch(() => {
        // Timeout or worker error: the worker was reset; show the text unformatted.
        if (!cancelled) setWorkerHtml(renderPlain(text));
      });
    return () => {
      cancelled = true;
    };
  }, [long, text, stable, off]);

  if (!long) return renderShort(text, stable);
  // While the worker is busy, keep showing the last result rather than flashing plain text.
  return off.cached(text, stable) ?? workerHtml ?? renderPlain(text.slice(0, 2000));
}

const INLINE_IMAGE = /^(?:data:image\/(?:png|jpe?g|gif|webp|avif|bmp|svg\+xml);|blob:)/i;

function hydrateEmbeds(
  root: ParentNode,
  resolveEmbed: (target: LinkTarget) => Promise<string | null>,
  isCancelled: () => boolean,
): void {
  const cards = root.querySelectorAll<HTMLElement>('.md-embed[data-md-embed]');
  cards.forEach((card) => {
    const target = targetFromAttrs((name) => card.getAttribute(name));
    if (!target) return;
    resolveEmbed(target)
      .then((content) => {
        if (content == null || isCancelled() || !card.isConnected) return;
        const body = card.querySelector<HTMLElement>('.md-embed-body');
        if (!body) return;
        if (card.dataset.mdEmbed === 'image') {
          if (!INLINE_IMAGE.test(content)) return;
          const img = document.createElement('img');
          img.className = 'md-image';
          img.alt = target.note ?? '';
          img.src = content;
          const size = /^(\d+)(?:x(\d+))?$/.exec(target.alias ?? '');
          if (size) {
            img.width = Number(size[1]);
            if (size[2]) img.height = Number(size[2]);
          }
          body.replaceChildren(img);
        } else {
          body.innerHTML = renderToHtml(content, { nested: true });
        }
        body.hidden = false;
        card.classList.add('md-embed-loaded');
      })
      .catch(() => {
        // A failed embed stays a plain link card.
      });
  });
}

function hydrateDiagrams(root: ParentNode, isCancelled: () => boolean): void {
  root.querySelectorAll<HTMLElement>('.md-mermaid[data-md-mermaid]').forEach((el) => {
    const source = el.querySelector('code')?.textContent ?? '';
    renderDiagram(source)
      .then((svg) => {
        if (isCancelled() || !el.isConnected) return;
        el.innerHTML = '';
        el.append(diagramShell(source, svg));
        el.classList.add('md-mermaid-rendered');
      })
      .catch((error: unknown) => {
        if (isCancelled() || !el.isConnected) return;
        el.classList.add('md-mermaid-error');
        const note = document.createElement('div');
        note.className = 'md-mermaid-message';
        note.textContent = error instanceof Error ? error.message : 'Diagram could not be rendered';
        el.prepend(note);
      });
  });
}

const ICON_COPY =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/></svg>';
const ICON_DONE =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';
const ICON_WRAP =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 6h18M3 12h14a3 3 0 0 1 0 6h-4m0 0 2-2m-2 2 2 2M3 18h5"/></svg>';
const ICON_CODE =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m16 18 6-6-6-6M8 6l-6 6 6 6"/></svg>';

function iconButton(kind: string, title: string, icon: string): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = `md-tool md-tool-${kind}`;
  button.dataset.mdTool = kind;
  button.title = title;
  button.setAttribute('aria-label', title);
  button.innerHTML = icon;
  return button;
}

/**
 * A rendered diagram in the same shell as a code block: a header naming the kind of diagram, a
 * toggle for its source and Copy, then the picture.
 */
function diagramShell(source: string, svg: string): HTMLElement {
  const shell = document.createElement('div');
  shell.className = 'md-diagram';
  const head = document.createElement('div');
  head.className = 'md-diagram-head';
  const kind = document.createElement('span');
  kind.className = 'md-diagram-kind';
  kind.textContent = diagramKind(source);
  const spacer = document.createElement('span');
  spacer.className = 'md-diagram-spacer';
  const toggle = iconButton('source', 'Show the source', ICON_CODE);
  toggle.setAttribute('aria-pressed', 'false');
  head.append(kind, spacer, toggle, iconButton('copy', 'Copy the source', ICON_COPY));
  const view = document.createElement('div');
  view.className = 'md-diagram-view';
  view.innerHTML = svg;
  const code = document.createElement('pre');
  code.className = 'md-diagram-source';
  code.hidden = true;
  const inner = document.createElement('code');
  inner.textContent = source;
  code.append(inner);
  shell.append(head, view, code);
  return shell;
}

/**
 * A wrap toggle and a Copy button in each code block's label bar. The HTML is a string, so the buttons are
 * added to the live DOM after each render; one click handler on the root serves them all.
 */
function hydrateCopyButtons(root: ParentNode): void {
  root.querySelectorAll<HTMLElement>('pre.md-code').forEach((pre) => {
    if (pre.closest('.md-mermaid') || pre.querySelector(':scope > .md-code-tools')) return;
    const bar = document.createElement('span');
    bar.className = 'md-code-tools';
    const wrap = iconButton('wrap', 'Wrap long lines', ICON_WRAP);
    wrap.setAttribute('aria-pressed', 'false');
    bar.append(wrap, iconButton('copy', 'Copy code', ICON_COPY));
    pre.appendChild(bar);
  });
}

/** One click on a code block's or diagram's tool button. */
function runTool(button: HTMLElement): void {
  const kind = button.dataset.mdTool;
  if (kind === 'copy') {
    const scope = button.closest('.md-diagram, pre.md-code');
    const code = scope?.querySelector('code')?.textContent ?? '';
    void navigator.clipboard?.writeText(code).then(() => {
      button.innerHTML = ICON_DONE;
      window.setTimeout(() => {
        button.innerHTML = ICON_COPY;
      }, 1500);
    });
  } else if (kind === 'wrap') {
    const on = button.closest('pre.md-code')?.classList.toggle('md-wrap') ?? false;
    button.setAttribute('aria-pressed', String(on));
  } else if (kind === 'source') {
    const source = button.closest('.md-diagram')?.querySelector<HTMLElement>('.md-diagram-source');
    if (!source) return;
    source.hidden = !source.hidden;
    button.setAttribute('aria-pressed', String(!source.hidden));
  }
}

/** Parses `html` into nodes without touching the live DOM. */
function fragment(html: string): DocumentFragment {
  const t = document.createElement('template');
  t.innerHTML = html;
  return t.content;
}

/**
 * Streaming text, rendered block by block (blocks.ts). Finished blocks are
 * appended once, hydrated once and never touched again; only the last block's nodes are replaced
 * as the text grows. Runs in a layout effect so each commit's text is in the DOM before it paints.
 */
function useIncremental(
  incremental: boolean,
  ref: { current: HTMLDivElement | null },
  text: string,
  streaming: boolean,
  interactive: boolean,
  resolveEmbed: MarkdownProps['resolveEmbed'],
): void {
  const state = useRef<{ blocks: StreamBlocks; tail: ChildNode[]; tailHtml: string; tailHydrated: boolean; cancelled: boolean } | null>(null);
  const embeds = useRef(resolveEmbed);
  embeds.current = resolveEmbed;

  useLayoutEffect(() => {
    const root = ref.current;
    if (!incremental || !root) return;
    const st = (state.current ??= { blocks: new StreamBlocks(), tail: [], tailHtml: '', tailHydrated: false, cancelled: false });
    const u = st.blocks.update(text, { streaming: true, interactive }, !streaming);
    if (u.reset) {
      root.replaceChildren();
      st.tail = [];
      st.tailHtml = '';
    }
    // The last block is only hydrated (Copy buttons, diagrams, embeds) once the stream has ended.
    const hydrateTail = !streaming;
    if (u.added.length === 0 && u.tail === st.tailHtml && st.tailHydrated === hydrateTail) return;
    const isCancelled = () => st.cancelled;
    const hydrate = (f: DocumentFragment) => {
      const resolve = embeds.current;
      if (resolve) hydrateEmbeds(f, resolve, isCancelled);
      if (interactive) {
        hydrateDiagrams(f, isCancelled);
        hydrateCopyButtons(f);
      }
    };
    for (const n of st.tail) n.remove();
    for (const html of u.added) {
      const f = fragment(html);
      hydrate(f);
      root.append(f);
    }
    const tail = fragment(u.tail);
    if (hydrateTail) hydrate(tail);
    st.tail = [...tail.childNodes];
    st.tailHtml = u.tail;
    st.tailHydrated = hydrateTail;
    root.append(tail);
  }, [incremental, ref, text, streaming, interactive]);

  useEffect(() => {
    // Development's StrictMode replays the mount: undo the replayed unmount's cancel.
    if (state.current) state.current.cancelled = false;
    return () => {
      if (state.current) state.current.cancelled = true;
    };
  }, []);
}

export function Markdown({
  text,
  streaming = false,
  interactive = false,
  onLink,
  resolveEmbed,
  className,
}: MarkdownProps) {
  // A text that is streaming when it mounts is rendered incrementally for the life of this
  // instance; everything else renders whole, through the shared cache (and the worker when long).
  const [incremental] = useState(streaming);
  const html = useRenderedHtml(incremental ? '' : text, { streaming, interactive });
  const ref = useRef<HTMLDivElement>(null);
  useIncremental(incremental, ref, text, streaming, interactive, resolveEmbed);

  useEffect(() => {
    const root = ref.current;
    if (!root || incremental) return;
    let cancelled = false;
    const isCancelled = () => cancelled;
    if (resolveEmbed) hydrateEmbeds(root, resolveEmbed, isCancelled);
    if (interactive) hydrateDiagrams(root, isCancelled);
    if (interactive) hydrateCopyButtons(root);
    return () => {
      cancelled = true;
    };
  }, [html, interactive, resolveEmbed, incremental]);

  const linkFor = (event: MouseEvent<HTMLElement>): LinkTarget | null => {
    const anchor = (event.target as Element).closest<HTMLElement>('a[data-md-kind]');
    if (!anchor || !event.currentTarget.contains(anchor)) return null;
    // The webview must never navigate by itself: every link goes through the app.
    event.preventDefault();
    return targetFromAttrs((name) => anchor.getAttribute(name));
  };

  return (
    <div
      ref={ref}
      className={className ? `md-root ${className}` : 'md-root'}
      onClick={(event) => {
        const tool = (event.target as Element).closest<HTMLElement>('[data-md-tool]');
        if (tool && event.currentTarget.contains(tool)) {
          runTool(tool);
          return;
        }
        const target = linkFor(event);
        if (target) onLink?.(target);
      }}
      onAuxClick={(event) => {
        linkFor(event);
      }}
      dangerouslySetInnerHTML={incremental ? undefined : { __html: html }}
    />
  );
}
