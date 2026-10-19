// Attachments by reference: files and folders as absolute paths,
// other chats by id. The host checks them and names them to the agent as data on send. Pure parts,
// so they are tested.

export interface AttachedRef {
  id: string;
  kind: 'file' | 'folder' | 'chat';
  name: string;
  /** The path, or the referenced chat's id. */
  target: string;
}

/** At most this many attachments (images, files, folders and chats together) per message. */
export const MAX_ATTACHMENTS = 20;

/** How many more may be added; 0 at the limit. */
export function roomLeft(images: number, refs: number): number {
  return Math.max(0, MAX_ATTACHMENTS - images - refs);
}

/** A new reference joins the list unless the host answered with one already there (same target). */
export function addRef(list: AttachedRef[], r: AttachedRef): AttachedRef[] {
  return list.some((x) => x.id === r.id) ? list : [...list, r];
}

export interface Box {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/**
 * A native drop position (physical pixels, from the host's `file_drop` event) in CSS pixels, and
 * whether it lands in the box. `ratio` is the window's device pixel ratio, which covers zoom too.
 */
export function dropLandsIn(box: Box | null, x: number, y: number, ratio: number): boolean {
  if (!box || !(ratio > 0)) return false;
  const cx = x / ratio;
  const cy = y / ratio;
  return cx >= box.left && cx <= box.right && cy >= box.top && cy <= box.bottom;
}

/** A `file_drop` event's fields, checked. */
export interface FileDrop {
  phase: 'enter' | 'over' | 'drop' | 'leave';
  paths: string[];
  x: number;
  y: number;
}

export function parseFileDrop(e: Record<string, unknown>): FileDrop | null {
  const phase = e.phase;
  if (phase !== 'enter' && phase !== 'over' && phase !== 'drop' && phase !== 'leave') return null;
  const paths = Array.isArray(e.paths) ? e.paths.filter((p): p is string => typeof p === 'string') : [];
  const x = typeof e.x === 'number' ? e.x : NaN;
  const y = typeof e.y === 'number' ? e.y : NaN;
  return { phase, paths, x, y };
}
