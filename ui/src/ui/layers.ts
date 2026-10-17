// The one Escape layer stack. Every surface that Escape can close
// registers a layer here instead of adding its own keydown handler. One capture-phase listener on
// the document routes each Escape to exactly one layer, in this order:
//
//   menus → popovers → clearing a search field → dialogs → panel layers → the composer's interrupt
//
// Within a kind the most recently pushed layer goes first. A layer may decline (its handler returns
// false, or `active()` says false), and the next one in order is asked. A busy dialog declines.

export type LayerKind = 'menu' | 'popover' | 'search' | 'dialog' | 'panel' | 'composer';

export const LAYER_ORDER: readonly LayerKind[] = ['menu', 'popover', 'search', 'dialog', 'panel', 'composer'];

export interface Layer {
  kind: LayerKind;
  /** Handle Escape. Return false to pass it on to the next layer. */
  onEscape: (e: KeyboardEvent | null) => boolean | void;
  /** Checked at Escape time: a search field with no text, a busy dialog, an idle composer. */
  active?: () => boolean;
}

interface Entry extends Layer {
  seq: number;
}

export class LayerStack {
  private readonly entries = new Set<Entry>();
  private seq = 0;

  /** Register a layer; the returned function removes it. */
  push(layer: Layer): () => void {
    const e: Entry = { ...layer, seq: ++this.seq };
    this.entries.add(e);
    return () => {
      this.entries.delete(e);
    };
  }

  /** The layers in the order Escape asks them. */
  ordered(): Layer[] {
    return [...this.entries].sort((a, b) => LAYER_ORDER.indexOf(a.kind) - LAYER_ORDER.indexOf(b.kind) || b.seq - a.seq);
  }

  /** Whether any layer of this kind is open (and active). */
  has(kind: LayerKind): boolean {
    return [...this.entries].some((e) => e.kind === kind && (e.active?.() ?? true));
  }

  /** Route one Escape. True when a layer took it. */
  escape(e: KeyboardEvent | null = null): boolean {
    for (const l of this.ordered()) {
      if (l.active && !l.active()) continue;
      if (l.onEscape(e) !== false) return true;
    }
    return false;
  }

  /** Install the single document listener. Returns the uninstaller. */
  install(target: Pick<Document, 'addEventListener' | 'removeEventListener'>): () => void {
    const onKey = (ev: Event) => {
      const e = ev as KeyboardEvent;
      if (e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
      if (this.escape(e)) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    target.addEventListener('keydown', onKey, true);
    return () => target.removeEventListener('keydown', onKey, true);
  }
}

/** The app's one stack. */
export const layers = new LayerStack();
