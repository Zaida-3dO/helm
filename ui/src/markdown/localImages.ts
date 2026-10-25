// Pictures on this computer that a reply or a tool card points at. The renderer marks them with
// their path; the component asks the host for the bytes (`local_image_get`) and shows a blob URL.
// The loader is set by the chat view, which has the app's API; the pure parts are tested.

/** The path of a local picture named by a Markdown image target, or null when it is anything else. */
export function localImagePath(href: string): string | null {
  let t = href.trim();
  if (/^file:\/\//i.test(t)) {
    t = t.replace(/^file:\/\/\/?/i, '');
    try {
      t = decodeURI(t);
    } catch {
      return null;
    }
    // file:///C:/x.png is C:/x.png; file:///home/me/x.png is /home/me/x.png
    if (!/^[A-Za-z]:/.test(t)) t = `/${t}`;
  }
  const absolute = /^[A-Za-z]:[\\/]/.test(t) || (t.startsWith('/') && !t.startsWith('//'));
  if (!absolute || !/\.(?:png|jpe?g|gif|webp)$/i.test(t)) return null;
  return t;
}

export type LocalImageLoader = (path: string) => Promise<{ mime: string; bytes: string }>;

let loader: LocalImageLoader | null = null;

/** Set by the chat view; returns the way to undo it. */
export function setLocalImageLoader(l: LocalImageLoader): () => void {
  loader = l;
  return () => {
    if (loader === l) loader = null;
  };
}

const urls = new Map<string, string>();
const KEPT = 40;

/** A blob URL for the picture, or a rejection when the host cannot show it. Recent ones are kept. */
export async function loadLocalImage(path: string): Promise<string> {
  const kept = urls.get(path);
  if (kept) return kept;
  if (!loader) throw new Error('Pictures from disk are not available here.');
  const r = await loader(path);
  const bin = atob(r.bytes);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  const url = URL.createObjectURL(new Blob([bytes], { type: r.mime }));
  urls.set(path, url);
  if (urls.size > KEPT) {
    const oldest = urls.keys().next().value;
    if (oldest !== undefined) {
      URL.revokeObjectURL(urls.get(oldest)!);
      urls.delete(oldest);
    }
  }
  return url;
}
