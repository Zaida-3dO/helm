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
