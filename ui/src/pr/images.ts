// Pictures in a pull request's description and comments. The page may not load addresses on
// GitHub's image hosts, so the host fetches each one (only one found in this pull request's own
// text) and the picture is drawn from the bytes it sends back.

import type { RefObject } from 'react';
import { useEffect } from 'react';
import type { PrEntry } from './store.ts';

const KEPT = 60;
const pictures = new Map<string, Promise<string>>();

function picture(entry: PrEntry, url: string): Promise<string> {
  const { owner, repo, number } = entry.ref;
  const key = `${owner}/${repo}#${number} ${url}`;
  const kept = pictures.get(key);
  if (kept) return kept;
  const loading = entry.client.image(entry.ref, url).then((r) => `data:${r.contentType};base64,${r.data}`);
  pictures.set(key, loading);
  // A failed fetch is not remembered: the address was signed for a few minutes only.
  loading.catch(() => pictures.delete(key));
  if (pictures.size > KEPT) {
    const oldest = pictures.keys().next().value;
    if (oldest !== undefined) pictures.delete(oldest);
  }
  return loading;
}

function draw(host: HTMLElement, entry: PrEntry, isLive: () => boolean): void {
  host.querySelectorAll<HTMLAnchorElement>('a.md-image-link:not([data-pr-image])').forEach((a) => {
    a.dataset.prImage = '1';
    const href = a.getAttribute('href') ?? '';
    if (!href.startsWith('https://')) return;
    const alt = a.textContent ?? '';
    picture(entry, href)
      .then((src) => {
        if (!isLive() || !a.isConnected) return;
        const img = document.createElement('img');
        img.className = 'md-image';
        img.alt = alt;
        img.src = src;
        a.replaceChildren(img);
      })
      .catch(() => {
        // The picture stays the link it was.
      });
  });
}

/** Replaces the image links inside `host` with the pictures themselves, as the text renders. */
export function useRemoteImages(host: RefObject<HTMLElement | null>, entry: PrEntry | undefined): void {
  useEffect(() => {
    const el = host.current;
    if (!el || !entry) return;
    let live = true;
    const run = () => draw(el, entry, () => live);
    run();
    const watch = new MutationObserver(run);
    watch.observe(el, { childList: true, subtree: true });
    return () => {
      live = false;
      watch.disconnect();
    };
  }, [host, entry]);
}
