// Typed wrappers over the board read ops (`board_read`, `board_cancel`). The host passes the
// board's JSON through, so these shapes name only the fields the Tasks view reads; everything else
// is ignored. Nothing here writes to the board.

import type { Api } from './client.ts';

export type BoardColumnId = 'backlog' | 'in_progress' | 'waiting' | 'completed';
export const BOARD_COLUMNS: readonly BoardColumnId[] = ['backlog', 'in_progress', 'waiting', 'completed'];
export const COLUMN_TITLE: Record<BoardColumnId, string> = {
  backlog: 'Backlog',
  in_progress: 'In progress',
  waiting: 'Waiting',
  completed: 'Completed',
};

export interface BoardLinkChip {
  key: string;
  url: string;
}

export interface BoardHolder {
  holderId: string;
  holderType?: string;
  displayName: string;
  role: string;
  roleCustom?: string | null;
  liveness?: string;
  lastActive?: string;
}

/** A live assignment from the fleet read: a holder plus where the work runs. */
export interface FleetAssignment extends BoardHolder {
  id?: string;
  itemId: string;
  machine?: string;
  branch?: string | null;
}

export interface BoardCardItem {
  id: string;
  title: string;
  state: string;
  headline: string | null;
  kind?: string;
  priority?: string;
  area?: string;
  repo?: string | null;
  blockedReason?: string | null;
  pauseReason?: string | null;
  links?: BoardLinkChip[];
}

export interface BoardEntry {
  item: BoardCardItem;
  column: BoardColumnId;
  assignments: BoardHolder[];
  subtasks: { total: number; done: number } | null;
}

export interface BoardSection {
  entries: BoardEntry[];
  total: number;
  nextCursor: string | null;
  withheld?: boolean;
}

export interface BoardPage {
  board: { columns: Partial<Record<BoardColumnId, BoardSection>>; notice?: string | null };
}

export interface BoardProjectRow {
  id: string;
  title: string;
  headline?: string | null;
  total?: number | null;
  finished?: number | null;
}

export interface ProjectsRead {
  projects: BoardProjectRow[];
  complete: boolean;
}

export interface FleetRead {
  assignments: FleetAssignment[];
  complete: boolean;
}

export interface ItemSummary {
  item: BoardCardItem & { checkpointHeadline?: string | null };
}

export interface ItemRecordLite extends BoardCardItem {
  body?: string;
  links?: BoardLinkChip[];
  parentId?: string | null;
}

export interface ItemDetail {
  detail: { item: ItemRecordLite; column?: BoardColumnId };
}

export interface HistoryEntry {
  id: string;
  ts: string;
  type: string;
  actorType?: string;
  actorId?: string | null;
  headline: string | null;
  body?: string | null;
}

export interface HistoryPage {
  entries: HistoryEntry[];
  total: number;
  nextCursor: string | null;
}

export interface ArtifactEntry {
  id: string;
  kind: string;
  verdict: string | null;
  ref: string | null;
  createdAt: string;
  body?: string | null;
  bodyPreview?: string | null;
  bodyTruncated?: boolean;
}

export interface ArtifactPage {
  artifacts: ArtifactEntry[];
  total: number;
  nextCursor: string | null;
}

export interface LoopEntry {
  loopId: string;
  kind: string;
  status: string;
  openedAt: string;
  text: string;
  textTruncated: boolean;
}

export interface LoopPage {
  loops: LoopEntry[];
  total: number;
  nextCursor: string | null;
}

export interface SearchMatch {
  id: string;
  title: string;
  state: string;
  headline: string | null;
}

export interface SearchRead {
  matches: SearchMatch[];
}

/** What one read asks for. `id` names the item for the item reads. */
export type ReadParams =
  | { action: 'projects' }
  | { action: 'board_page'; column: BoardColumnId; project?: string; limit?: number; cursor?: string }
  | { action: 'item'; id: string }
  | { action: 'detail'; id: string; historyLimit?: number; artifactLimit?: number }
  | { action: 'history'; id: string; limit?: number; cursor?: string }
  | { action: 'artifacts'; id: string; limit?: number; cursor?: string }
  | { action: 'loops'; id: string; limit?: number; cursor?: string; includeClosed?: boolean }
  | { action: 'loop'; id: string; loopId: string }
  | { action: 'fleet' }
  | { action: 'ready' }
  | { action: 'search'; query: string; limit?: number };

export type ReadResult<A extends ReadParams['action']> = A extends 'projects'
  ? ProjectsRead
  : A extends 'board_page'
    ? BoardPage
    : A extends 'item'
      ? ItemSummary
      : A extends 'detail'
        ? ItemDetail
        : A extends 'history'
          ? HistoryPage
          : A extends 'artifacts'
            ? ArtifactPage
            : A extends 'loops'
              ? LoopPage
              : A extends 'loop'
                ? { loopId: string; text: string }
                : A extends 'fleet'
                  ? FleetRead
                  : A extends 'ready'
                    ? { ready: boolean }
                    : A extends 'search'
                      ? SearchRead
                      : never;

export interface BoardConnectionInfo {
  url: string;
  hasToken: boolean;
}

export function boardOps(api: Api) {
  return {
    connection: () => api<BoardConnectionInfo>('board_connection_get', {}),
    read: (params: ReadParams, requestId: string, extra: { timeoutMs?: number; attachOnly?: boolean } = {}) =>
      api<{ action: string; data: unknown }>('board_read', { ...params, requestId, ...extra }),
    cancel: (requestId: string) => api<{ cancelled: boolean }>('board_cancel', { requestId }),
  };
}

export type BoardOps = ReturnType<typeof boardOps>;
