// Boards are where chats are shown; folders are only where they run. These are the host calls for
// the default board and for making a new board project, kept free of React so they are tested under
// node:test.

import type { Api } from '../api/client.ts';
import type { ProjectNode } from '../api/projects.ts';

/** The default board: where a chat with no natural home goes (a quick question, a message from the
 *  phone, a scheduled run), and the boards every picker leaves out. */
export interface DefaultBoardState {
  /** The board in use: the one chosen, else the fallback the host found. */
  boardId: string | null;
  name: string | null;
  /** The board the user chose, or null when the host's fallback is in use. */
  chosen: string | null;
  /** Boards no picker lists (the catch-all inbox). */
  hiddenBoardIds: string[];
}

export const NO_DEFAULT_BOARD: DefaultBoardState = { boardId: null, name: null, chosen: null, hiddenBoardIds: [] };

/** The default board, or none when the host cannot say (no board connected yet, or an older host). */
export async function getDefaultBoard(api: Api): Promise<DefaultBoardState> {
  try {
    return normalise(await api<DefaultBoardState>('default_board_get', {}));
  } catch {
    return NO_DEFAULT_BOARD;
  }
}

/** Choose the default board; null goes back to the host's fallback. */
export async function setDefaultBoard(api: Api, boardId: string | null): Promise<DefaultBoardState> {
  return normalise(await api<DefaultBoardState>('default_board_set', { boardId }));
}

function normalise(s: Partial<DefaultBoardState> | null | undefined): DefaultBoardState {
  return {
    boardId: s?.boardId ?? null,
    name: s?.name ?? null,
    chosen: s?.chosen ?? null,
    hiddenBoardIds: Array.isArray(s?.hiddenBoardIds) ? s.hiddenBoardIds : [],
  };
}

export interface NewBoardProject {
  title: string;
  description?: string;
  /** The folder the board's new chats start in. */
  folder?: string;
}

/** Make a new project on the task board. It arrives as a board, with its folder set when one was
 *  given. Empty fields are left out. */
export function createBoardProject(api: Api, a: NewBoardProject): Promise<ProjectNode> {
  const title = a.title.trim();
  const description = a.description?.trim() ?? '';
  const folder = a.folder?.trim() ?? '';
  return api<ProjectNode>('board_project_create', { title, ...(description ? { description } : {}), ...(folder ? { folder } : {}) });
}
