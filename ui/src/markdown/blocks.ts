// Block-incremental rendering for streaming Markdown. A streaming reply
// grows at its end, so only its last top-level block can still change. The finished blocks before
// it are rendered once each and handed back as new HTML to append; only the last block is
// re-rendered on each update. The cost of an update is the size of the last block, not of the
// whole reply.
//
// A top-level block is finished once the lexer starts another non-blank block after it and that
// block's first line is complete: Markdown cannot reach back across such a boundary (a list ends
// where an unindented paragraph starts, a fence runs to its closing fence or to the end). Reference
// link definitions can, so they are carried forward to every later block; a definition that only
// arrives after the block using it is the one difference from a full render, and the next full
// render (a remount) shows it.
//
// Text that starts with frontmatter is rendered whole each time (rare in replies). Text that is
// rewritten rather than extended starts again from scratch. Pure: no DOM.

import { lexBlocks, renderToHtml, renderTokens, type LinkDefs, type RenderOptions } from './render.ts';

export interface BlockUpdate {
  /** The earlier output no longer applies (the text was rewritten): clear it before appending. */
  reset: boolean;
  /** HTML of blocks that finished with this update, in order. Append; never re-rendered. */
  added: string[];
  /**
   * HTML of the last block, replacing the previous tail. Empty when there is none. It stays the
   * tail even once the stream is done, because a paced reveal can still be adding words to it.
   */
  tail: string;
}

const FRONTMATTER = /^---[ \t]*\n/;

export class StreamBlocks {
  /** The text the finished blocks were rendered from. */
  private prefix = '';
  private links: LinkDefs = {};
  /** Blocks handed out so far; for tests and diagnostics. */
  finished = 0;

  /**
   * Bring the output up to `text`. `done` once the stream has ended: the last block is then
   * rendered as finished (a closed Mermaid fence becomes a diagram), but it is still the tail.
   */
  update(raw: string, options: RenderOptions = {}, done = false): BlockUpdate {
    const text = raw.replace(/\r\n?/g, '\n');
    const final: RenderOptions = { ...options, streaming: false };
    let reset = false;
    if (!text.startsWith(this.prefix)) {
      reset = true;
      this.prefix = '';
      this.links = {};
    }
    if (this.prefix === '' && FRONTMATTER.test(text)) return { reset, added: [], tail: renderToHtml(text, done ? final : options) };
    const tokens = lexBlocks(text.slice(this.prefix.length), this.links);
    if (!tokens) return { reset, added: [], tail: '' };
    // Everything before the last non-blank block is finished. Until that block's first line is
    // complete it may still turn out to continue the one before (a "2" that becomes "2. second"
    // in a loose list), so both stay open.
    const blocks: number[] = [];
    for (let i = 0; i < tokens.length; i++) if (tokens[i]!.type !== 'space') blocks.push(i);
    let cut = blocks.length ? blocks[blocks.length - 1]! : 0;
    if (cut > 0 && !tokens[cut]!.raw.includes('\n')) cut = blocks.length > 1 ? blocks[blocks.length - 2]! : 0;
    const added: string[] = [];
    let consumed = 0;
    for (let i = 0; i < cut; i++) {
      const t = tokens[i]!;
      consumed += t.raw.length;
      if (t.type === 'space') continue;
      // Only a finished definition is carried forward: one still arriving ("[r]: htt") must not
      // win over its complete self, and marked keeps the first definition of a name.
      if (t.type === 'def') this.links[t.tag] ??= { href: t.href, title: t.title };
      const html = renderTokens([t], tokens.links, final);
      if (html) added.push(html);
      this.finished++;
    }
    this.prefix = text.slice(0, this.prefix.length + consumed);
    const rest = tokens.slice(cut);
    return { reset, added, tail: rest.length ? renderTokens(rest, tokens.links, done ? final : options) : '' };
  }
}
