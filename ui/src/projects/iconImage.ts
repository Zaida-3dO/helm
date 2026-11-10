// The rules behind a project's icon picture and the three kinds of project icon. Pure functions, no
// DOM, so they are tested under node:test; the picker draws with them.
//
// A project's `icon` is one of: nothing (the default glyph), an emoji, `lib:<name>` (one of the
// library line icons) or `img` (a small picture the host keeps).

/** The biggest picture file the picker will read. */
export const IMAGE_MAX_BYTES = 5 * 1024 * 1024;
/** The saved picture is this many pixels on a side. */
export const ICON_PX = 64;
/** The biggest encoded picture the host takes. */
export const PNG_MAX_BYTES = 64 * 1024;
export const ACCEPTED_TYPES = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'];

export type IconChoice = 'default' | 'emoji' | 'lib' | 'img';

/** Why a picked file cannot be used, in plain words; null when it can. */
export function imageProblem(file: { type: string; size: number }): string | null {
  if (!ACCEPTED_TYPES.includes(file.type)) return 'Pick a PNG, JPEG, WebP or GIF picture.';
  if (file.size <= 0) return 'That file is empty.';
  if (file.size > IMAGE_MAX_BYTES) return 'That picture is over 5 MB; pick a smaller one.';
  return null;
}

/**
 * The square to cut from a `w` x `h` picture. At zoom 1 it is the largest square; a bigger zoom
 * makes it smaller. `cx` and `cy` (0 to 1) say where its centre sits across the picture; the
 * square is kept inside the picture. All results are whole pixels.
 */
export function squareCrop(w: number, h: number, zoom = 1, cx = 0.5, cy = 0.5): { sx: number; sy: number; side: number } {
  if (!(w >= 1) || !(h >= 1)) return { sx: 0, sy: 0, side: 0 };
  const z = Number.isFinite(zoom) && zoom > 1 ? zoom : 1;
  const side = Math.max(1, Math.min(Math.floor(Math.min(w, h)), Math.round(Math.min(w, h) / z)));
  const place = (centre: number, extent: number) => {
    const c = Number.isFinite(centre) ? Math.min(1, Math.max(0, centre)) : 0.5;
    return Math.min(extent - side, Math.max(0, Math.round(c * extent - side / 2)));
  };
  return { sx: place(cx, w), sy: place(cy, h), side };
}

/** The decoded size of a base64 string, without decoding it. */
export function base64Bytes(b64: string): number {
  const s = b64.replace(/\s+/g, '');
  if (s.length === 0) return 0;
  const pad = s.endsWith('==') ? 2 : s.endsWith('=') ? 1 : 0;
  return Math.max(0, Math.floor((s.length * 3) / 4) - pad);
}

/** The kind of icon a stored value is. An empty `lib:` or a plain word reads as the default. */
export function iconChoice(icon: string | null | undefined): IconChoice {
  if (!icon) return 'default';
  if (icon === 'img') return 'img';
  if (icon.startsWith('lib:')) return icon.length > 4 ? 'lib' : 'default';
  return /[A-Za-z0-9]/.test(icon) ? 'default' : 'emoji';
}

/** The library name inside `lib:<name>`; null for any other value. */
export function libName(icon: string | null | undefined): string | null {
  return icon && icon.startsWith('lib:') && icon.length > 4 ? icon.slice(4) : null;
}
