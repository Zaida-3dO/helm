// Image attachments in the composer: paste or drop images, shown as blob:
// thumbnails; each is stored with `attachment_put` and its id rides the send. The host checks
// the same limits again; these checks only save a round trip.

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
  /** The picture itself, kept so a send that is taken back can attach it again. */
  file?: Blob;
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

/** What a send carried, kept so taking the message back (withdraw, recover, edit) can restore it. */
export interface SentAttachments {
  files: Blob[];
  /** Files, folders and chats by reference, as the host named them: a path or a chat id. */
  refs: Array<{ kind: 'path' | 'chat'; target: string }>;
}

const KEEP_SENDS = 8;
/** Most bytes of pictures kept for taking sends back; the oldest sends go first. */
const KEEP_BYTES = 100 * 1024 * 1024;
const sent = new Map<string, SentAttachments & { at: number }>();

/** Remember a send's attachments under its submission id (only the last few are kept). */
export function stashSent(submissionId: string, s: SentAttachments, now = Date.now()): void {
  if (!s.files.length && !s.refs.length) return;
  sent.set(submissionId, { ...s, at: now });
  const bytes = () => [...sent.values()].reduce((n, x) => n + x.files.reduce((m, f) => m + f.size, 0), 0);
  while (sent.size > 1 && (sent.size > KEEP_SENDS || bytes() > KEEP_BYTES)) sent.delete(sent.keys().next().value!);
}

/** Forget one send's attachments (it was delivered, or its chat closed). */
export function forgetSent(submissionId: string): void {
  sent.delete(submissionId);
}

/**
 * Forget the sends among `ids` that are no longer waiting: not in `waiting` (the chat's unsent
 * submissions) and older than `graceMs`, which covers the moment before the host lists a new send.
 */
export function forgetDelivered(ids: Iterable<string>, waiting: ReadonlySet<string>, now = Date.now(), graceMs = 5000): void {
  for (const id of ids) {
    const s = sent.get(id);
    if (s && !waiting.has(id) && now - s.at > graceMs) sent.delete(id);
  }
}

/** The attachments of a send, once: null when it carried none or is forgotten. */
export function takeSent(submissionId: string): SentAttachments | null {
  const s = sent.get(submissionId) ?? null;
  sent.delete(submissionId);
  return s;
}
