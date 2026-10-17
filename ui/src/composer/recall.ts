// Prompt recall: Up on an empty composer steps back through this chat's earlier
// prompts, Down steps forward, and Down past the newest returns to an empty composer. Any other
// edit ends the session. Pure, so the stepping rules are tested without an editor.

export class Recall {
  private prompts: string[] = [];
  private index = -1;

  /** True while a recalled prompt is shown. */
  get active(): boolean {
    return this.index >= 0;
  }

  /** Step older. `prompts` are newest first. Returns the text to show, or null to do nothing. */
  up(prompts: readonly string[], current: string): string | null {
    if (!this.active) {
      if (current.trim() !== '' || prompts.length === 0) return null;
      this.prompts = [...prompts];
      this.index = 0;
      return this.prompts[0]!;
    }
    if (current !== this.prompts[this.index]) {
      this.end();
      return null;
    }
    if (this.index + 1 >= this.prompts.length) return null;
    this.index += 1;
    return this.prompts[this.index]!;
  }

  /** Step newer; past the newest the composer empties and the session ends. */
  down(current: string): string | null {
    if (!this.active) return null;
    if (current !== this.prompts[this.index]) {
      this.end();
      return null;
    }
    this.index -= 1;
    if (this.index < 0) {
      this.end();
      return '';
    }
    return this.prompts[this.index]!;
  }

  /** The user edited, clicked or left: the session ends. */
  end(): void {
    this.index = -1;
    this.prompts = [];
  }
}

/** This chat's prompts for recall, newest first: active user messages, no repeats. */
export function recallPrompts(rows: readonly { role: string; text: string; status: string }[]): string[] {
  const out: string[] = [];
  for (let i = rows.length - 1; i >= 0; i--) {
    const r = rows[i]!;
    if (r.role !== 'user' || r.status !== 'complete' || !r.text.trim()) continue;
    if (out[out.length - 1] === r.text) continue;
    out.push(r.text);
  }
  return out;
}
