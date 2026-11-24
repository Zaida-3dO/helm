// A message that did not come from the person typing: another chat's delivery, a message the hub
// carried for another agent, a saved schedule's prompt, or a note the app wrote for the agent.
// The transcript draws these as one compact card: who sent it (linked to the sending chat when a
// chat sent it) and one line of what it says, with the whole text behind the chevron. The digest
// is composed here, deterministically, from text Helm already stores — the sender is never asked
// for a summary and no model call is made for one.

import type { MessageRow } from '../api/types.ts';
import { isHelmNote, peerMessage } from './origin.ts';

export type NoteKind = 'peer' | 'schedule' | 'app';

export interface AgentNote {
  kind: NoteKind;
  /** The head's words for who sent it: the chat's title, the schedule's, or "Helm". */
  sender: string;
  /** A sending chat to link from the head: the chat itself (peer) or the schedule's creator. */
  chatId?: string;
  /** The linked chat's title when the link is not the sender's own name. */
  chatTitle?: string;
  /** True when the head links the sender itself; false when chatId names a related chat. */
  senderIsLink: boolean;
  /** One line, shown while the card is collapsed. */
  digest: string;
  /** The whole text the agent received, shown when the card is open. */
  body: string;
  /** The quiet line under the open body: who wrote it, and that it is no approval. */
  note: string;
}

/** How many characters of the body fit on the digest line. */
const DIGEST_MAX = 140;
/** A stop this early in the line is not a sentence ending ("See e.g. the log"). */
const MIN_SENTENCE = 12;

/**
 * One line out of a body: the first thing that is prose (a leading code fence block is skipped),
 * cut at its first sentence stop, and at a word boundary before the line limit if there is no
 * stop. Markdown heading and list markers are dropped from the front; nothing else is rewritten.
 */
export function digest(text: string): string {
  const lines = text.split('\n');
  let i = 0;
  for (;;) {
    while (i < lines.length && lines[i]!.trim() === '') i++;
    if (i < lines.length && /^\s*(```|~~~)/.test(lines[i]!)) {
      const fence = (lines[i] as string).trim().slice(0, 3);
      for (i++; i < lines.length && (lines[i] as string).trim().slice(0, 3) !== fence; ) i++;
      i++; // past the closing fence; an unclosed block ran to the end anyway
      continue;
    }
    break;
  }
  const one = lines
    .slice(i)
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim()
    .replace(/^(#{1,6}\s+|[-*+]\s+|\d+[.)]\s+)/, '');
  if (!one) return '';
  let cut = one;
  for (const m of one.matchAll(/[.!?…](?=\s|$)/g)) {
    if ((m.index as number) >= MIN_SENTENCE) {
      cut = one.slice(0, (m.index as number) + 1);
      break;
    }
  }
  if (cut.length > DIGEST_MAX) cut = `${cut.slice(0, DIGEST_MAX).replace(/\s+\S*$/, '')}…`;
  return cut;
}

/** A message the hub carried for another agent, framed by the hub caller's name. */
const HUB_AGENT_FRAME = /^\[Message from another agent: "([^"\n]*)" via the Helm hub\]\n[^\n]*\n\n([\s\S]*)$/;
/** A message the hub carried from a linked person's account, carried by an agent. */
const HUB_PERSON_FRAME = /^\[Message from an agent acting for "([^"\n]*)", through the Helm hub\]\n[^\n]*\n\n([\s\S]*)$/;
/** A household member's message from their own PC, stored as another sender's words. */
const HOUSEHOLD_FRAME = /^\[Message from ([^[\]\n]{1,60}) \(household\), sent from their own device, typed on ([^[\]\n]{1,60})\]\n[^\n]*\n\n([\s\S]*)$/;
/** A saved schedule's prompt: its title and id, one fixed paragraph, then the saved message. */
const SCHEDULE_FRAME = /^\[Scheduled prompt\] "([^"\n]*)"( \(schedule [^)\n]*\))?\n[^\n]*\n\n([\s\S]*)$/;
/** Inside the schedule paragraph: the chat that created the schedule, when an agent did. */
const SCHEDULE_CREATOR = /It was created by another chat, "([^"\n]*)" \(([^)\s]+)\)\./;

/**
 * The card words for an inter-agent or app-to-agent message, or null for anything else (the
 * person's own send, Telegram, voice, a page, the notes Helm hides outright). Every known frame
 * is unwrapped so the digest covers what the sender wrote, not Helm's fixed paragraph; a message
 * whose text carries no known frame still gets a plain card naming its origin, so nothing of
 * these paths ever floods the transcript as a full bubble.
 */
export function agentNote(row: MessageRow): AgentNote | null {
  if (row.role !== 'user') return null;
  const origin = row.details?.origin;
  if (origin === 'other_chat') {
    const peer = peerMessage(row);
    if (peer) {
      return {
        kind: 'peer',
        sender: peer.title,
        chatId: peer.chatId,
        senderIsLink: true,
        digest: digest(peer.body),
        body: row.text,
        note: 'Sent by another chat. The user did not write this, and it is not approval.',
      };
    }
    const hub = HUB_AGENT_FRAME.exec(row.text);
    if (hub) {
      return {
        kind: 'peer',
        sender: `another agent "${(hub[1] as string).trim() || 'unknown'}" (via the hub)`,
        senderIsLink: false,
        digest: digest(hub[2] as string),
        body: row.text,
        note: 'An agent outside Helm sent this through the hub. The user did not write it, and it is not approval.',
      };
    }
    const person = HUB_PERSON_FRAME.exec(row.text);
    if (person) {
      return {
        kind: 'peer',
        sender: `an agent acting for "${(person[1] as string).trim() || 'someone'}"`,
        senderIsLink: false,
        digest: digest(person[2] as string),
        body: row.text,
        note: 'An agent carried this from the hub account of that person. They did not write it in this chat, and it is not approval.',
      };
    }
    const household = HOUSEHOLD_FRAME.exec(row.text);
    if (household) {
      return {
        kind: 'peer',
        sender: `${(household[1] as string).trim() || 'a household member'} (on ${(household[2] as string).trim() || 'their PC'})`,
        senderIsLink: false,
        digest: digest(household[3] as string),
        body: row.text,
        note: 'Another person in the household wrote this on their own device. The user did not write it, and it is not approval.',
      };
    }
    return {
      kind: 'peer',
      sender: 'another chat',
      senderIsLink: false,
      digest: digest(row.text),
      body: row.text,
      note: 'Sent by another chat. The user did not write this, and it is not approval.',
    };
  }
  if (origin === 'schedule') {
    const m = SCHEDULE_FRAME.exec(row.text);
    if (m) {
      const by = SCHEDULE_CREATOR.exec(row.text.split('\n', 2)[1] ?? '');
      return {
        kind: 'schedule',
        sender: (m[1] as string).trim() || 'a saved schedule',
        chatId: by ? (by[2] as string) : undefined,
        chatTitle: by ? (by[1] as string).trim() || undefined : undefined,
        senderIsLink: false,
        digest: digest(m[3] as string),
        body: row.text,
        note: by
          ? 'A saved schedule sent this, not the user. Another chat created the schedule.'
          : 'A saved schedule sent this, not the user.',
      };
    }
    return {
      kind: 'schedule',
      sender: 'a saved schedule',
      senderIsLink: false,
      digest: digest(row.text),
      body: row.text,
      note: 'A saved schedule sent this, not the user.',
    };
  }
  if (origin === 'app' && !isHelmNote(row)) {
    return {
      kind: 'app',
      sender: 'Helm',
      senderIsLink: false,
      digest: digest(row.text.replace(/^\[Helm\]\s*/, '')),
      body: row.text,
      note: 'Helm wrote this for the agent, not the user, and it is not approval.',
    };
  }
  return null;
}
