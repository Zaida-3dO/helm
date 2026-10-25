// Composer slots: other areas add accessories and send contributions without
// editing the composer.
//
// - registerComposerAccessory: drawn above the editor (quick replies) or in the toolbar
//   (the panel-context eye toggle and its "Looking at: …" chip).
// - composerApi(chatId).insert(text): put text into a chat's draft without sending (review
//   comments' "Add to message", option-button amendments). Returns false when that chat's
//   composer is not mounted.
// - registerSendContributor: called on every composer send; what it returns rides the send
//   (the panel-context payload).

import type { ReactNode } from 'react';
import type { ChatRecord, ThreadState } from '../api/types.ts';
import type { SentAttachments } from './attachments.ts';

export interface AccessoryContext {
  chat: ChatRecord;
  thread: ThreadState | null;
  /** True while the chat runs a turn. */
  busy: boolean;
  /** True when the editor holds no text. */
  draftEmpty: boolean;
}

export interface ComposerAccessory {
  id: string;
  /** `dock`: the composer dock's stack above the box (the review tray). */
  /** `consent`: the Allow cards, first in the dock. */
  placement: 'above' | 'toolbar' | 'dock' | 'consent';
  /** For `toolbar`: the left group (model, chips; the default) or the right one (actions). */
  side?: 'start' | 'end';
  render: (ctx: AccessoryContext) => ReactNode;
}

export interface SendContribution {
  /** The panel-context payload; the host builds the block from it. */
  context?: unknown;
}

export type SendContributor = (chatId: string) => SendContribution | undefined;

const accessories: ComposerAccessory[] = [];
const contributors: SendContributor[] = [];
interface MountedComposer {
  insert: (text: string) => void;
  focus: () => void;
  /** Attach again what a taken-back message carried. */
  restore?: (sent: SentAttachments) => void;
}
const mounted = new Map<string, MountedComposer>();

export function registerComposerAccessory(a: ComposerAccessory): () => void {
  const i = accessories.findIndex((x) => x.id === a.id);
  if (i >= 0) accessories[i] = a;
  else accessories.push(a);
  return () => {
    const j = accessories.indexOf(a);
    if (j >= 0) accessories.splice(j, 1);
  };
}

export function accessoriesAt(placement: ComposerAccessory['placement']): ComposerAccessory[] {
  return accessories.filter((a) => a.placement === placement);
}

/** Which toolbar group an accessory joins: the panel-context eye sits with the
 *  actions on the right until its own registration says so. */
export function accessorySide(a: Pick<ComposerAccessory, 'id' | 'side'>): 'start' | 'end' {
  return a.side ?? (a.id === 'panel-context' ? 'end' : 'start');
}

export function registerSendContributor(f: SendContributor): () => void {
  contributors.push(f);
  return () => {
    const j = contributors.indexOf(f);
    if (j >= 0) contributors.splice(j, 1);
  };
}

/** Every contributor's part for one send, merged (a later `context` wins). */
export function collectSendContributions(chatId: string): SendContribution {
  const out: SendContribution = {};
  for (const f of contributors) {
    try {
      const c = f(chatId);
      if (c && c.context !== undefined) out.context = c.context;
    } catch (e) {
      console.error('a send contributor failed', e);
    }
  }
  return out;
}

/** Called by the composer when it mounts for a chat; returns the unmount. */
export function mountComposer(chatId: string, handle: MountedComposer): () => void {
  mounted.set(chatId, handle);
  return () => {
    if (mounted.get(chatId) === handle) mounted.delete(chatId);
  };
}

export function composerApi(chatId: string) {
  return {
    /** Append text to the draft (after a blank line when the draft has text). Never sends. */
    insert(text: string): boolean {
      const h = mounted.get(chatId);
      if (!h) return false;
      h.insert(text);
      return true;
    },
    /** Put a taken-back message's images and files into the composer again. */
    restore(sent: SentAttachments): boolean {
      const h = mounted.get(chatId);
      if (!h?.restore) return false;
      h.restore(sent);
      return true;
    },
    focus(): boolean {
      const h = mounted.get(chatId);
      if (!h) return false;
      h.focus();
      return true;
    },
  };
}
