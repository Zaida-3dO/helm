// Image attachments in the composer: paste or drop images, shown as blob:
// thumbnails; each is stored with `attachment_put` and its id rides the send. The host checks
// the same limits again; these checks only save a round trip.
//
// The host is the one place an attachment lives: a staged one stays in the chat's unstaged set
// (`attachment_list`) until it is sent, and comes back to that set when a send is withdrawn,
// recovered or never got sent. The composer stages from the host, so attachments survive a
// restart — nothing is kept in renderer memory between opens.

export const MAX_PER_MESSAGE = 20;
export const MAX_IMAGE_BYTES = 25 * 1024 * 1024;
export const MAX_IMAGE_TOTAL = 60 * 1024 * 1024;
export const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/webp'];

export interface Attached {
  id: string;
  name: string;
  size: number;
  /** A blob: URL for the thumbnail; revoke it when the attachment goes. */
  url: string;
}

export interface Candidate {
  name: string;
  type: string;
  size: number;
}

/** Why a file cannot be added to `current`, or null when it can. */
export function refusal(current: readonly Attached[], f: Candidate): string | null {
  if (f.type === 'image/gif') return `${f.name}: a GIF goes as a file reference. Attach it with the paperclip, or drop it from Explorer.`;
  if (!IMAGE_TYPES.includes(f.type)) return `${f.name}: only PNG, JPEG and WebP images can be pasted. Attach other files with the paperclip, or drop them from Explorer.`;
  if (current.length >= MAX_PER_MESSAGE) return 'A message can carry at most 20 attachments.';
  if (f.size > MAX_IMAGE_BYTES) return `${f.name} is larger than 25 MB.`;
  const total = current.reduce((n, a) => n + a.size, 0) + f.size;
  if (total > MAX_IMAGE_TOTAL) return 'A message can carry at most 60 MB of images.';
  return null;
}

/**
 * Whether these bytes are an animated PNG (an `acTL` chunk ahead of the image data) or an animated
 * WebP (the animation flag in its extended header). Only still images are sent inline.
 */
export function isAnimated(mime: string, b: Uint8Array): boolean {
  const tag = (at: number, s: string) => s.split('').every((c, i) => b[at + i] === c.charCodeAt(0));
  if (mime === 'image/png' && b.length > 8 && b[0] === 0x89 && tag(1, 'PNG')) {
    for (let at = 8; at + 8 <= b.length; ) {
      if (tag(at + 4, 'acTL')) return true;
      if (tag(at + 4, 'IDAT') || tag(at + 4, 'IEND')) return false;
      // The length is unsigned; a corrupt one that runs past the end ends the scan.
      const next = at + 12 + (((b[at]! << 24) | (b[at + 1]! << 16) | (b[at + 2]! << 8) | b[at + 3]!) >>> 0);
      if (next > b.length) return false;
      at = next;
    }
    return false;
  }
  if (mime === 'image/webp' && b.length > 21 && tag(0, 'RIFF') && tag(8, 'WEBP') && tag(12, 'VP8X')) return (b[20]! & 0x02) !== 0;
  return false;
}

/** The file's bytes as base64 (chunked, so a large image does not overflow the call stack). */
export async function toBase64(file: Blob): Promise<string> {
  return bytesToBase64(new Uint8Array(await file.arrayBuffer()));
}

export function bytesToBase64(bytes: Uint8Array): string {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

/**
 * The host's unstaged pool entries the composer is not showing yet (matched by id): what a
 * reopening composer stages, and what a taken-back send's message brings back.
 */
export function unstagedOnly<T extends { id: string }>(pool: readonly T[], staged: readonly { id: string }[]): T[] {
  return pool.filter((x) => !staged.some((s) => s.id === x.id));
}
