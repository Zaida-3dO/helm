// The pure renderer: Markdown text in, safe HTML out. No DOM, no network, no `api`.
//
// Safety model: the HTML is built here, not passed through. Raw HTML in the source is
// reduced to inert, escaped markers; link destinations pass an allowlist; images are never
// emitted with a remote `src`. Callers can therefore assign the result to `innerHTML` as is.
import { Lexer, Marked, Renderer } from 'marked';
import type { Token, Tokens, TokensList, TokenizerAndRendererExtension } from 'marked';
import { canonicalCalloutType, defaultCalloutTitle, parseCalloutHead } from './callouts.ts';
import { escapeAttr, escapeHtml } from './escape.ts';
import { splitFrontmatter } from './frontmatter.ts';
import type { Property } from './frontmatter.ts';
import { highlightCode } from './highlight.ts';
import { classifyHref, isImageName, parseWikilink, targetAttrs } from './links.ts';
import { localImagePath } from './localImages.ts';
import { pathInCode, pathInProse, PROSE_PATH_RUN, trimProseRun } from './pathrefs.ts';
import type { LinkTarget } from './links.ts';

export interface RenderOptions {
  /** The last block may be unfinished: an open Mermaid fence stays a code block. */
  streaming?: boolean;
  /** Enables Mermaid diagrams. Off for previews. */
  interactive?: boolean;
  /**
   * The text is itself an embedded note: frontmatter is dropped and embeds inside it render as
   * plain links, so embedding stays one level deep.
   */
  nested?: boolean;
}

/** Set for the duration of one synchronous render. */
let ctx: Required<RenderOptions> = { streaming: false, interactive: false, nested: false };

function attrString(attrs: Array<[string, string]>): string {
  return attrs.map(([k, v]) => ` ${k}="${escapeAttr(v)}"`).join('');
}

function anchor(target: LinkTarget, innerHtml: string, extraClass = '', title = ''): string {
  const href = target.kind === 'url' ? (target.href ?? '#') : '#';
  const cls = `md-link md-link-${target.kind}${extraClass ? ` ${extraClass}` : ''}`;
  const titleAttr = title ? ` title="${escapeAttr(title)}"` : '';
  return (
    `<a class="${cls}" href="${escapeAttr(href)}" rel="noopener noreferrer"` +
    `${attrString(targetAttrs(target))}${titleAttr}>${innerHtml}</a>`
  );
}

function vaultLabel(target: LinkTarget): string {
  if (target.alias) return target.alias;
  const base = target.note ?? '';
  const tail = target.heading ? ` > ${target.heading}` : target.block ? ` > ^${target.block}` : '';
  return `${base}${tail}` || '#';
}

const wikilink: TokenizerAndRendererExtension = {
  name: 'wikilink',
  level: 'inline',
  start(src: string) {
    const at = src.indexOf('[[');
    if (at < 0) return undefined;
    return at > 0 && src[at - 1] === '!' ? at - 1 : at;
  },
  tokenizer(src: string) {
    const m = /^(!?)\[\[([^[\]\n]+)\]\]/.exec(src);
    if (!m) return undefined;
    const target = parseWikilink(m[2] as string);
    if (!target) return undefined;
    return { type: 'wikilink', raw: m[0], embed: m[1] === '!', target };
  },
  renderer(token) {
    const target = (token as unknown as { target: LinkTarget }).target;
    const embed = (token as unknown as { embed: boolean }).embed;
    const label = escapeHtml(vaultLabel(target));
    if (!embed || ctx.nested) return anchor(target, label, embed ? 'md-embed-link' : '');
    const image = isImageName(target.note ?? '');
    const kind = image ? 'image' : 'note';
    const attrs = attrString(targetAttrs(target));
    // Spans, not divs: an embed sits inside a paragraph. The body is filled in later, from
    // `resolveEmbed`, by the component.
    return (
      `<span class="md-embed md-embed-${kind}" data-md-embed="${kind}"${attrs}>` +
      `${anchor(target, label, 'md-embed-title')}<span class="md-embed-body" hidden></span></span>`
    );
  },
};

/** A boundary before a prose path: start of text, whitespace or an opening bracket or quote. */
const PATH_LEAD = /[\s(["']/;

/** Plain-text file paths become `path` links. Inside link text, never. */
const pathref: TokenizerAndRendererExtension = {
  name: 'pathref',
  level: 'inline',
  start(src: string) {
    // Report the boundary character before a candidate, so the tokenizer can see it.
    const re = /[\s(["'](?=[~./\\A-Za-z0-9_@-])/g;
    for (let m = re.exec(src); m; m = re.exec(src)) {
      const run = PROSE_PATH_RUN.exec(src.slice(m.index + 1));
      // Every prose path form has a slash; ordinary words skip the full check.
      if (run && /[\\/]/.test(run[0]) && pathInProse(trimProseRun(run[0]))) return m.index;
    }
    return undefined;
  },
  tokenizer(src: string) {
    if ((this as unknown as { lexer: { state: { inLink: boolean } } }).lexer.state.inLink) return undefined;
    const lead = src.length > 0 && PATH_LEAD.test(src.charAt(0)) ? src.charAt(0) : '';
    const run = PROSE_PATH_RUN.exec(src.slice(lead.length));
    if (!run) return undefined;
    const text = trimProseRun(run[0]);
    if (!pathInProse(text)) return undefined;
    return { type: 'pathref', raw: lead + text, lead, text };
  },
  renderer(token) {
    const t = token as unknown as { lead: string; text: string };
    return `${escapeHtml(t.lead)}${anchor({ kind: 'path', href: t.text }, escapeHtml(t.text), 'md-path')}`;
  },
};

function fenceClosed(raw: string): boolean {
  const lines = raw.replace(/\s+$/, '').split('\n');
  const open = /^ {0,3}(`{3,}|~{3,})/.exec(lines[0] ?? '');
  if (!open || lines.length < 2) return false;
  const fence = open[1] as string;
  const close = new RegExp(`^ {0,3}\\${fence[0]}{${fence.length},}[ \\t]*$`);
  return close.test(lines[lines.length - 1] as string);
}

const SAFE_DATA_IMAGE = /^data:image\/(?:png|jpe?g|gif|webp|avif|bmp);base64,[a-z0-9+/=]+$/i;

function renderPropertyValues(prop: Property): string {
  if (prop.values.length === 0) return '<span class="md-prop-empty"></span>';
  const items = prop.values.map((v) => md.parseInline(v) as string);
  if (!prop.list) return items.join('');
  return `<ul class="md-prop-list">${items.map((i) => `<li>${i}</li>`).join('')}</ul>`;
}

function renderProperties(props: Property[]): string {
  const rows = props
    .map((p) => {
      const key = p.key ? `<dt>${escapeHtml(p.key)}</dt>` : '<dt></dt>';
      return `<div class="md-prop">${key}<dd>${renderPropertyValues(p)}</dd></div>`;
    })
    .join('');
  return (
    '<details class="md-properties"><summary>Properties</summary>' +
    `<dl class="md-properties-list">${rows}</dl></details>\n`
  );
}

// Math: `$x^2$` inline, `$$ ... $$` as its own block. The source stays in the markup as text; the
// component turns it into MathML afterwards (like a diagram), so this renderer stays plain
// strings. An opening `$` must touch its text and a closing `$` must not be followed by a digit,
// so prices ("$5 and $10") stay prices.
const mathInline: TokenizerAndRendererExtension = {
  name: 'mathInline',
  level: 'inline',
  start(src: string) {
    const at = src.indexOf('$');
    return at < 0 ? undefined : at;
  },
  tokenizer(src: string) {
    const m = /^\$\$([^\n]+?)\$\$/.exec(src) ?? /^\$(?![\s$])((?:\\.|[^$\\\n])+?)(?<![\s\\])\$(?!\d)/.exec(src);
    if (!m) return undefined;
    return { type: 'mathInline', raw: m[0], text: (m[1] as string).trim(), display: m[0].startsWith('$$') };
  },
  renderer(token) {
    const t = token as unknown as { text: string; display: boolean };
    const kind = t.display ? 'display' : 'inline';
    return `<span class="md-math md-math-${kind}" data-md-math="${kind}"><span class="md-math-src">${escapeHtml(t.text)}</span></span>`;
  },
};

const mathBlock: TokenizerAndRendererExtension = {
  name: 'mathBlock',
  level: 'block',
  start(src: string) {
    const at = src.search(/(?:^|\n) {0,3}\$\$/);
    return at < 0 ? undefined : at;
  },
  tokenizer(src: string) {
    const m = /^ {0,3}\$\$[ \t]*\n([\s\S]+?)\n[ \t]*\$\$[ \t]*(?:\n|$)/.exec(src);
    if (!m) return undefined;
    return { type: 'mathBlock', raw: m[0], text: (m[1] as string).trim() };
  },
  renderer(token) {
    const t = token as unknown as { text: string };
    return `<div class="md-math md-math-display" data-md-math="display"><span class="md-math-src">${escapeHtml(t.text)}</span></div>\n`;
  },
};

const md = new Marked({ gfm: true, breaks: false, async: false });

md.use({
  extensions: [wikilink, pathref, mathBlock, mathInline],
  renderer: {
    // Raw HTML becomes an inert marker. <br> is the one tag agents lean on in table cells.
    html(token: Tokens.HTML | Tokens.Tag) {
      const text = token.text.trim();
      if (/^<br\s*\/?>$/i.test(text)) return '<br>';
      if (/^<!--[\s\S]*-->$/.test(text)) return '';
      if (token.block) {
        return `<pre class="md-raw-html"><code>${escapeHtml(text)}</code></pre>\n`;
      }
      return `<code class="md-raw-html">${escapeHtml(token.text)}</code>`;
    },

    link(this: Renderer, token: Tokens.Link) {
      const inner = this.parser.parseInline(token.tokens);
      const target = classifyHref(token.href);
      if (!target) return inner;
      return anchor(target, inner, '', token.title ?? '');
    },

    // Inline code that is a whole file path is also a `path` link.
    codespan(token: Tokens.Codespan) {
      const code = `<code>${escapeHtml(token.text)}</code>`;
      return pathInCode(token.text) ? anchor({ kind: 'path', href: token.text.trim() }, code, 'md-path') : code;
    },

    // Never an <img> with a remote or relative src: the app fetches nothing it was not asked to.
    // (A local picture has no src at all until the host answers.)
    image(token: Tokens.Image) {
      if (SAFE_DATA_IMAGE.test(token.href)) {
        const title = token.title ? ` title="${escapeAttr(token.title)}"` : '';
        return `<img class="md-image" src="${escapeAttr(token.href)}" alt="${escapeAttr(token.text)}"${title}>`;
      }
      // A picture on this computer: marked with its path, and drawn once the host has sent it.
      const local = localImagePath(token.href);
      if (local) {
        return `<img class="md-image md-image-local" data-md-local-src="${escapeAttr(local)}" alt="${escapeAttr(token.text)}">`;
      }
      const label = escapeHtml(token.text || token.href);
      const target = classifyHref(token.href);
      return target ? anchor(target, label, 'md-image-link', token.title ?? '') : label;
    },

    code(token: Tokens.Code) {
      const lang = (token.lang ?? '').trim().split(/\s+/)[0] ?? '';
      const safeLang = lang.replace(/[^A-Za-z0-9_+#.-]/g, '');
      if (
        safeLang.toLowerCase() === 'mermaid' &&
        ctx.interactive &&
        !ctx.nested &&
        (!ctx.streaming || fenceClosed(token.raw))
      ) {
        // The source stays visible as a code block until the component swaps in the SVG.
        return (
          '<div class="md-mermaid" data-md-mermaid>' +
          `<pre class="md-code"><code>${escapeHtml(token.text)}</code></pre></div>\n`
        );
      }
      const langAttr = safeLang ? ` data-lang="${escapeAttr(safeLang)}"` : '';
      const langClass = safeLang ? ` language-${escapeAttr(safeLang)}` : '';
      return (
        `<pre class="md-code"${langAttr}><code class="hljs${langClass}">` +
        `${highlightCode(token.text, safeLang.toLowerCase())}</code></pre>\n`
      );
    },

    table(this: Renderer, token: Tokens.Table) {
      const html = Renderer.prototype.table.call(this, token);
      return `<div class="md-table-wrap">${html}</div>\n`;
    },

    blockquote(this: Renderer, token: Tokens.Blockquote) {
      const first = token.tokens[0];
      const head = first && first.type === 'paragraph' ? parseCalloutHead(first.text) : null;
      if (!head) return false;

      const canon = canonicalCalloutType(head.type);
      const titleHtml = head.title
        ? (md.parseInline(head.title) as string)
        : escapeHtml(defaultCalloutTitle(head.type));
      const restHtml = head.rest.trim() ? `<p>${md.parseInline(head.rest) as string}</p>\n` : '';
      const bodyHtml = restHtml + this.parser.parse(token.tokens.slice(1));
      const content = bodyHtml.trim() ? `<div class="md-callout-content">${bodyHtml}</div>` : '';
      const dataType = escapeAttr(head.type.toLowerCase());
      const cls = `md-callout md-callout-${canon}`;
      if (head.fold === '') {
        return (
          `<div class="${cls}" data-callout="${dataType}">` +
          `<div class="md-callout-title">${titleHtml}</div>${content}</div>\n`
        );
      }
      const open = head.fold === '+' ? ' open' : '';
      return (
        `<details class="${cls}" data-callout="${dataType}"${open}>` +
        `<summary class="md-callout-title">${titleHtml}</summary>${content}</details>\n`
      );
    },
  },
});

/** Escaped text on a plain block: what the UI shows when rendering fails or times out. */
export function renderPlain(text: string): string {
  return `<p class="md-plain">${escapeHtml(text)}</p>`;
}

/** Renders Markdown (with the Obsidian quirks) to safe HTML. Never throws. */
export function renderToHtml(text: string, options: RenderOptions = {}): string {
  const previous = ctx;
  ctx = {
    streaming: options.streaming ?? false,
    interactive: options.interactive ?? false,
    nested: options.nested ?? false,
  };
  try {
    const normalised = text.replace(/\r\n?/g, '\n');
    const { properties, body } = splitFrontmatter(normalised, ctx.streaming);
    const head = properties && !ctx.nested ? renderProperties(properties) : '';
    return head + (md.parse(body) as string);
  } catch {
    return renderPlain(text);
  } finally {
    ctx = previous;
  }
}

// Block-level pieces for incremental streaming (blocks.ts): the same lexer and renderer as
// renderToHtml, split so that finished top-level blocks are rendered once and only the last,
// unfinished block is rendered again as the text grows.

/** Link reference definitions collected by the lexer (`[x]: url`). */
export type LinkDefs = TokensList['links'];

/** The top-level tokens of already normalised Markdown (no frontmatter handling). Never throws. */
export function lexBlocks(text: string, links: LinkDefs = {}): TokensList | null {
  try {
    // marked resolves `[x]` reference links while lexing, so definitions from earlier blocks are
    // seeded into the lexer before it reads this text.
    const lexer = new Lexer(md.defaults);
    Object.assign(lexer.tokens.links, links);
    return lexer.lex(text);
  } catch {
    return null;
  }
}

/** Renders a run of top-level tokens, resolving reference links against `links`. Never throws. */
export function renderTokens(tokens: Token[], links: LinkDefs, options: RenderOptions = {}): string {
  const previous = ctx;
  ctx = {
    streaming: options.streaming ?? false,
    interactive: options.interactive ?? false,
    nested: options.nested ?? false,
  };
  try {
    const list = Object.assign([...tokens], { links }) as TokensList;
    return md.parser(list) as string;
  } catch {
    return renderPlain(tokens.map((t) => t.raw).join(''));
  } finally {
    ctx = previous;
  }
}
