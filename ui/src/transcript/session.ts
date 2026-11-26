// A chat that rolled over is one window over several sessions. Each row belongs to the session
// that wrote it, and the host keeps a row's pictures, turn changes, fork point and the rest under
// that session's id. These are the rules for telling a row of the window's own session from a row
// of an earlier one.

/** True for a row an earlier session wrote: its session is archived, so it cannot be edited or resent. */
export function isEarlierSession(row: { chatId: string }, windowChatId: string): boolean {
  return row.chatId !== windowChatId;
}

/**
 * The session a selection anchors in: the owner of the anchor row, or the window's chat when the
 * row is not loaded. A side chat forks from this session, since only it holds the row.
 */
export function anchorSession(rows: readonly { id: number; chatId: string }[], anchorId: number, windowChatId: string): string {
  return rows.find((r) => r.id === anchorId)?.chatId ?? windowChatId;
}
