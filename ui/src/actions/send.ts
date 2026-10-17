// How an action button, a chip, an option button or "Explain plainly" sends.
//
// All of them are the user's own sends, so they follow the chat's send behaviour exactly
// (`action: "default"`: a prompt when idle, a steer when busy), and none of them touches the
// draft: `origin: "action"` tells the host to ignore the draft revision and leave it alone.
// Helm never runs a command itself: the text is a message for the agent.

import { ApiError, type Api } from '../api/client.ts';
import type { ActionButton } from '../api/actions.ts';

/** Sent as the draft revision so even a host that still reads it keeps the draft (it is never <=). */
export const NO_DRAFT_REVISION = -1;

export interface ActionSendArgs {
  chatId: string;
  submissionId: string;
  text: string;
  action: 'default';
  origin: 'action';
  draftRevision: typeof NO_DRAFT_REVISION;
}

const uid = () => globalThis.crypto.randomUUID();

export function buildActionSend(chatId: string, text: string, submissionId: string = uid()): ActionSendArgs {
  return { chatId, submissionId, text, action: 'default', origin: 'action', draftRevision: NO_DRAFT_REVISION };
}

/** Send `text` into `chatId` as an action. Resolves when the host accepted it. */
export async function sendActionText(api: Api, chatId: string, text: string): Promise<void> {
  await api('chat_send', buildActionSend(chatId, text));
}

/** The prompt "Explain plainly" sends from a reply's message action. */
export const EXPLAIN_PLAINLY_TEXT = 'Please explain your last reply in plain language, avoiding jargon, and say what decision you need from me.';

/** A failure with a sentence the user can read. */
export class ActionError extends Error {}

export interface RunContext {
  /** The chat in view, if any. */
  chatId: string | null;
  /** Called with the new chat's id after a `new_chat` button created it. */
  select: (chatId: string) => void;
}

interface ProjectLike {
  id: string;
  path?: string | null;
}

function message(e: unknown): string {
  return e instanceof ApiError || e instanceof Error ? e.message : String(e);
}

/**
 * Run one button. A `prompt` button sends its text into the chat in view; a `new_chat` button
 * starts a chat in its bound project and sends the text there. Neither runs anything locally.
 */
export async function runButton(api: Api, button: ActionButton, ctx: RunContext): Promise<void> {
  if (button.kind === 'prompt') {
    if (!ctx.chatId) throw new ActionError('Open a chat first.');
    try {
      await sendActionText(api, ctx.chatId, button.text);
    } catch (e) {
      throw new ActionError(`Could not send "${button.label}": ${message(e)}`);
    }
    return;
  }
  if (!button.projectId) throw new ActionError(`"${button.label}" is not bound to a project yet. Bind it in Settings, Actions.`);
  let cwd: string | null | undefined;
  try {
    const projects = await api<ProjectLike[]>('project_list', {});
    cwd = projects.find((p) => p.id === button.projectId)?.path;
  } catch (e) {
    throw new ActionError(`Could not look up the project for "${button.label}": ${message(e)}`);
  }
  if (!cwd) throw new ActionError(`The project bound to "${button.label}" is gone or has no folder. Rebind it in Settings, Actions.`);
  try {
    const chat = await api<{ id: string }>('chat_create', { cwd });
    await sendActionText(api, chat.id, button.text);
    ctx.select(chat.id);
  } catch (e) {
    throw new ActionError(`Could not start a chat for "${button.label}": ${message(e)}`);
  }
}
