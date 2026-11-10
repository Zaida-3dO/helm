// "Show in Files": a request from a file tab (or the keyboard) that the Files tree picks up when it
// shows, plus a short-lived memory of each chat's project folder so a file tab can tell whether the
// file belongs to the workspace without asking on every render.

import { useEffect, useState } from 'react';
import type { PanelOps } from '../../api/panel.ts';
import { showSlot } from '../registry.ts';
import { isInside } from './paths.ts';

export interface Reveal {
  chatId: string;
  path: string;
  at: number;
}

/** A request nobody picked up this long ago is dropped. */
const REVEAL_TTL_MS = 10_000;
const ROOT_TTL_MS = 60_000;

let pending: Reveal | null = null;

/** Switches to the Files tab and leaves the file for the tree to expand to, select and focus. */
export function requestReveal(chatId: string, path: string): void {
  pending = { chatId, path, at: Date.now() };
  showSlot('files');
}

/** The tree calls this when it shows: the request for this chat, once. */
export function takeReveal(chatId: string): Reveal | null {
  const r = pending;
  if (!r || r.chatId !== chatId || Date.now() - r.at > REVEAL_TTL_MS) return null;
  pending = null;
  return r;
}

const roots = new Map<string, { at: number; value: Promise<string | null> }>();
/** The answers that have arrived, so a tab shown again does not flash a loading state. */
const resolved = new Map<string, string | null>();

/** The chat's project folder (null: none), asked of the host at most once a minute. */
export function chatRoot(ops: Pick<PanelOps, 'chatFolder'>, chatId: string): Promise<string | null> {
  const hit = roots.get(chatId);
  if (hit && Date.now() - hit.at < ROOT_TTL_MS) return hit.value;
  const value = ops
    .chatFolder(chatId)
    .then((r) => r.path)
    .catch(() => null)
    .then((path) => {
      resolved.set(chatId, path);
      return path;
    });
  roots.set(chatId, { at: Date.now(), value });
  return value;
}

/** The chat's project folder: undefined while it is being asked, null when there is none. */
export function useChatRoot(ops: Pick<PanelOps, 'chatFolder'>, chatId: string | null): string | null | undefined {
  const [got, setGot] = useState<{ chatId: string; root: string | null } | null>(null);
  useEffect(() => {
    if (!chatId) return;
    let cancelled = false;
    void chatRoot(ops, chatId).then((root) => !cancelled && setGot({ chatId, root }));
    return () => {
      cancelled = true;
    };
  }, [ops, chatId]);
  if (!chatId) return null;
  // Never another chat's answer; an earlier answer for this chat avoids a flash of "loading".
  return got?.chatId === chatId ? got.root : resolved.get(chatId);
}

/** Why Show in Files is unavailable for `path`, or null when it can be used. */
export function revealRefusal(root: string | null | undefined, path: string): string | null {
  if (root === undefined) return 'Checking this workspace…';
  return root && isInside(root, path) ? null : 'Not in this workspace';
}
