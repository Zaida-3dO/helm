// The sidebar's undo stack (50 entries, in memory only).
// Archives, moves, nests and reorders push an entry; Ctrl+Z undoes the newest, Ctrl+Shift+Z redoes.
// A new action clears redo.

export const UNDO_LIMIT = 50;

export interface UndoEntry {
  label: string;
  undo: () => Promise<void>;
  redo: () => Promise<void>;
}

export class UndoStack {
  private done: UndoEntry[] = [];
  private undone: UndoEntry[] = [];

  push(e: UndoEntry): void {
    this.done.push(e);
    if (this.done.length > UNDO_LIMIT) this.done.shift();
    this.undone = [];
  }

  get size(): number {
    return this.done.length;
  }

  canRedo(): boolean {
    return this.undone.length > 0;
  }

  async undo(): Promise<UndoEntry | null> {
    const e = this.done.pop();
    if (!e) return null;
    await e.undo();
    this.undone.push(e);
    return e;
  }

  async redo(): Promise<UndoEntry | null> {
    const e = this.undone.pop();
    if (!e) return null;
    await e.redo();
    this.done.push(e);
    return e;
  }
}

/** The one stack the sidebar and the top bar's chat menu share, so Ctrl+Z undoes either. */
export const sidebarUndo = new UndoStack();
