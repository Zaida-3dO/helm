// What the dev fake host answers for `attachment_get` with `thumb: true`: like the real host, a copy
// scaled down to fit 480 by 360 and never scaled up. It needs the browser's canvas; where there is
// none (the unit tests, an old browser) or the image cannot be read, the stored image is answered.

import { bytesFromBase64 } from '../transcript/messageImages.ts';

export const FAKE_THUMB_MAX_WIDTH = 480;
export const FAKE_THUMB_MAX_HEIGHT = 360;

/** The size that fits `w` by `h` into the box, keeping the aspect ratio and never scaling up. */
export function fitWithin(w: number, h: number): { width: number; height: number } {
  const scale = Math.min(1, FAKE_THUMB_MAX_WIDTH / w, FAKE_THUMB_MAX_HEIGHT / h);
  return { width: Math.max(1, Math.round(w * scale)), height: Math.max(1, Math.round(h * scale)) };
}

function toBase64(bytes: Uint8Array): string {
  let bin = '';
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

export async function fakeThumb(mime: string, base64: string): Promise<{ mime: string; bytes: string }> {
  const stored = { mime, bytes: base64 };
  if (typeof OffscreenCanvas === 'undefined' || typeof createImageBitmap === 'undefined') return stored;
  try {
    const bitmap = await createImageBitmap(new Blob([bytesFromBase64(base64)], { type: mime }));
    const { width, height } = fitWithin(bitmap.width, bitmap.height);
    if (width === bitmap.width && height === bitmap.height) return stored;
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext('2d');
    if (!ctx) return stored;
    ctx.drawImage(bitmap, 0, 0, width, height);
    const blob = await canvas.convertToBlob({ type: 'image/png' });
    return { mime: 'image/png', bytes: toBase64(new Uint8Array(await blob.arrayBuffer())) };
  } catch {
    return stored;
  }
}
