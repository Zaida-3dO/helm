// Going back to before one of the user's own messages: what the host says about it
// (`checkpoint_info`), who can be gone back to, and the plain sentences the confirmation shows.

import type { MessageRow } from '../api/types.ts';

/** The host's answer: what going back to a message means. */
export interface CheckpointInfo {
  /** The message's text, which the copy's draft will hold. */
  prompt: string;
  /** The user's messages from this one on, this one included: what the copy leaves out. */
  messagesAfter: number;
  /** Files changed from this message on, as far as the chat recorded them. */
  changedFiles: string[];
  /** Changed files beyond the ones listed. */
  moreFiles: number;
  /** Always false: only the conversation is restored. */
  filesRestored: boolean;
}

/** Only a complete message the user sent is a checkpoint. */
export function canGoBackTo(row: MessageRow): boolean {
  return row.role === 'user' && row.status === 'complete';
}

/** How many pictures the message carries (they do not come back with the text). */
export function imageCount(row: MessageRow): number {
  return row.details?.images?.length ?? 0;
}

/** The sentences before the file list, in the order they are shown. */
export function checkpointNotes(info: CheckpointInfo, images: number): string[] {
  const notes = [
    'Helm makes a copy of this chat as it was just before this message, with the message in the box so you can change it and send it again. This chat stays exactly as it is.',
  ];
  if (info.messagesAfter > 1) notes.push(`The copy leaves out this message and the ${info.messagesAfter - 1} you sent after it.`);
  if (images > 0) notes.push(images === 1 ? 'The picture on this message is not brought back.' : `The ${images} pictures on this message are not brought back.`);
  notes.push(
    info.filesRestored
      ? 'Files are put back too.'
      : 'Only the conversation comes back. Your files are not restored: they stay as the agent left them.',
  );
  return notes;
}

/** The line above the changed-file list, or the line that says there is none. */
export function filesHeading(info: CheckpointInfo): string {
  return info.changedFiles.length > 0
    ? 'The agent changed these files after this message. Check them, or undo the changes yourself if you want the old versions:'
    : 'No file changes were recorded after this message.';
}

/** "and 3 more" for the files the list leaves out; empty when none. */
export function moreFilesLine(info: CheckpointInfo): string {
  return info.moreFiles > 0 ? `and ${info.moreFiles} more` : '';
}
