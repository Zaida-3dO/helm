// The pure parts of the images in a user's bubble: how big a thumbnail is, and turning the host's
// base64 answer back into bytes. The component is MessageImages.tsx.

/** The largest thumbnail box, in CSS pixels. */
export const THUMB_MAX_WIDTH = 240;
export const THUMB_MAX_HEIGHT = 180;
/** A thumbnail never gets thinner than this, so a very wide image stays clickable. */
export const THUMB_MIN_SIDE = 48;

/**
 * The thumbnail's size: the image's own aspect ratio, scaled down to fit the box and never scaled
 * up. Computed before the bytes arrive so the row does not jump when the image loads.
 */
export function thumbSize(width: number, height: number): { width: number; height: number } {
  if (!(width > 0) || !(height > 0)) return { width: THUMB_MAX_HEIGHT, height: THUMB_MAX_HEIGHT };
  const scale = Math.min(1, THUMB_MAX_WIDTH / width, THUMB_MAX_HEIGHT / height);
  return {
    width: Math.max(THUMB_MIN_SIDE, Math.round(width * scale)),
    height: Math.max(THUMB_MIN_SIDE, Math.round(height * scale)),
  };
}

/** Base64 to bytes (the host's `attachment_get` answers base64). */
export function bytesFromBase64(b64: string): Uint8Array<ArrayBuffer> {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** What a screen reader hears for one image. */
export function imageLabel(name: string, index: number, count: number): string {
  const n = name.trim() === '' ? 'image' : name.trim();
  return count > 1 ? `${n} (image ${index + 1} of ${count})` : n;
}

/**
 * Thumbnail object URLs kept for a while after their row scrolls out of view, so scrolling back
 * shows the picture without asking the host again. A URL in use is never released; once nothing
 * uses it, it stays among the `max` most recently released and is revoked when pushed out.
 */
export class UrlCache {
  private readonly entries = new Map<string, { url: string; users: number }>();

  private readonly max: number;
  private readonly revoke: (url: string) => void;

  constructor(max: number, revoke: (url: string) => void) {
    this.max = max;
    this.revoke = revoke;
  }

  /** The kept URL for `key`, counted as in use until `release`; undefined when there is none. */
  acquire(key: string): string | undefined {
    const e = this.entries.get(key);
    if (!e) return undefined;
    e.users += 1;
    // Most recently used last, so the oldest idle entry is the first to go.
    this.entries.delete(key);
    this.entries.set(key, e);
    return e.url;
  }

  /** The kept URL for `key` without counting a use (for a first render); undefined when none. */
  peek(key: string): string | undefined {
    return this.entries.get(key)?.url;
  }

  /**
   * Keep a new URL for `key`, counted as in use, and answer the URL to show. When another user
   * already kept one for `key`, that one is shared (and counted) and returned instead; the caller
   * then owns `url` and revokes it.
   */
  put(key: string, url: string): string {
    const e = this.entries.get(key);
    if (e) {
      e.users += 1;
      this.entries.delete(key);
      this.entries.set(key, e);
      return e.url;
    }
    this.entries.set(key, { url, users: 1 });
    return url;
  }

  release(key: string): void {
    const e = this.entries.get(key);
    if (!e || e.users === 0) return;
    e.users -= 1;
    if (e.users === 0) this.trim();
  }

  private trim(): void {
    const idle = [...this.entries].filter(([, e]) => e.users === 0);
    for (const [key, e] of idle.slice(0, Math.max(0, idle.length - this.max))) {
      this.revoke(e.url);
      this.entries.delete(key);
    }
  }

  get size(): number {
    return this.entries.size;
  }
}
