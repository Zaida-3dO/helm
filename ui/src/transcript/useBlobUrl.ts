// One picture's object URL, fetched from the host when the component mounts and let go when it
// leaves. `cache` (optional) keeps recent URLs after a component leaves, so scrolling back shows
// the picture without asking the host again; without one the URL belongs to the component.

import { useEffect, useRef, useState } from 'react';
import { UrlCache, bytesFromBase64 } from './messageImages.ts';

export type Loaded = { state: 'loading' } | { state: 'ready'; url: string } | { state: 'failed' };

export function revokeUrl(url: string): void {
  URL.revokeObjectURL(url);
}

/** `key` names the picture (the same key shares one URL in the cache); `fetchImage` asks the host for it. */
export function useBlobUrl(key: string, cache: UrlCache | null, fetchImage: () => Promise<{ mime: string; bytes: string }>): Loaded {
  const fetchRef = useRef(fetchImage);
  fetchRef.current = fetchImage;
  // A kept URL shows on the first render; the effect below takes its use.
  const [loaded, setLoaded] = useState<Loaded>(() => {
    const kept = cache?.peek(key);
    return kept ? { state: 'ready', url: kept } : { state: 'loading' };
  });
  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    const kept = cache?.acquire(key);
    if (kept) {
      setLoaded({ state: 'ready', url: kept });
    } else {
      setLoaded({ state: 'loading' });
      fetchRef
        .current()
        .then((r) => {
          if (cancelled) return;
          url = URL.createObjectURL(new Blob([bytesFromBase64(r.bytes)], { type: r.mime }));
          if (cache) {
            // Another component may have fetched the same picture meanwhile: share its URL, drop ours.
            const shared = cache.put(key, url);
            if (shared !== url) revokeUrl(url);
            url = shared;
          }
          setLoaded({ state: 'ready', url });
        })
        .catch(() => {
          if (!cancelled) setLoaded({ state: 'failed' });
        });
    }
    return () => {
      cancelled = true;
      if (cache) {
        if (kept || url) cache.release(key);
      } else if (url) {
        revokeUrl(url);
      }
    };
  }, [cache, key]);
  return loaded;
}
