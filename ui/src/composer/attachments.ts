// Image attachments in the composer: paste or drop images, shown as blob:
// thumbnails; each is stored with `attachment_put` and its id rides the send. The host checks
// the same limits again; these checks only save a round trip.

export const MAX_PER_MESSAGE = 20;
export const MAX_IMAGE_BYTES = 25 * 1024 * 1024;
export const MAX_IMAGE_TOTAL = 60 * 1024 * 1024;
export const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];

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
  if (!IMAGE_TYPES.includes(f.type)) return `${f.name}: only PNG, JPEG, GIF and WebP images can be attached.`;
  if (current.length >= MAX_PER_MESSAGE) return 'A message can carry at most 20 attachments.';
  if (f.size > MAX_IMAGE_BYTES) return `${f.name} is larger than 25 MB.`;
  const total = current.reduce((n, a) => n + a.size, 0) + f.size;
  if (total > MAX_IMAGE_TOTAL) return 'A message can carry at most 60 MB of images.';
  return null;
}

/** The file's bytes as base64 (chunked, so a large image does not overflow the call stack). */
export async function toBase64(file: Blob): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}
