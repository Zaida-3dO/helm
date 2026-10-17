import { useEffect, useMemo, useRef, useState } from 'react';
import type { MouseEvent } from 'react';
import { LONG_TEXT_CHARS, renderShort, sharedOffloader } from './engine.ts';
import { targetFromAttrs } from './links.ts';
import type { LinkTarget } from './links.ts';
import { renderDiagram } from './mermaid.ts';
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
  root: HTMLElement,
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

function hydrateDiagrams(root: HTMLElement, isCancelled: () => boolean): void {
  root.querySelectorAll<HTMLElement>('.md-mermaid[data-md-mermaid]').forEach((el) => {
    const source = el.querySelector('code')?.textContent ?? '';
    renderDiagram(source)
      .then((svg) => {
        if (isCancelled() || !el.isConnected) return;
        el.innerHTML = svg;
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

export function Markdown({
  text,
  streaming = false,
  interactive = false,
  onLink,
  resolveEmbed,
  className,
}: MarkdownProps) {
  const html = useRenderedHtml(text, { streaming, interactive });
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const root = ref.current;
    if (!root) return;
    let cancelled = false;
    const isCancelled = () => cancelled;
    if (resolveEmbed) hydrateEmbeds(root, resolveEmbed, isCancelled);
    if (interactive) hydrateDiagrams(root, isCancelled);
    return () => {
      cancelled = true;
    };
  }, [html, interactive, resolveEmbed]);

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
        const target = linkFor(event);
        if (target) onLink?.(target);
      }}
      onAuxClick={(event) => {
        linkFor(event);
      }}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
