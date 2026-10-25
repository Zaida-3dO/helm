import assert from 'node:assert/strict';
import { test } from 'node:test';
import { renderToHtml } from './render.ts';

const ATTR = String.raw`[^\s"'<>/=]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s"'<>=]+))?`;
const TAG = new RegExp(String.raw`<(/?)([a-z][a-z0-9-]*)((?:\s+${ATTR})*)\s*/?>`, 'gi');
const ATTR_PARTS = /([^\s"'<>/=]+)(?:\s*=\s*("[^"]*"|'[^']*'|[^\s"'<>=]+))?/g;

/**
 * Parses the markup the way a browser would see it and lists every element and attribute.
 * A `<` that does not start a well-formed tag fails: the renderer must only produce clean markup.
 */
function markup(html: string): Array<{ tag: string; attrs: Array<[string, string]> }> {
  const out: Array<{ tag: string; attrs: Array<[string, string]> }> = [];
  const leftover = html.replace(TAG, (_m, close: string, tag: string, attrs: string) => {
    if (!close) {
      const list: Array<[string, string]> = [];
      for (const a of attrs.matchAll(ATTR_PARTS)) {
        list.push([(a[1] as string).toLowerCase(), (a[2] ?? '').replace(/^["']|["']$/g, '')]);
      }
      out.push({ tag: tag.toLowerCase(), attrs: list });
    }
    return '';
  });
  assert.ok(!leftover.includes('<'), `malformed markup left over: ${leftover.slice(0, 80)}`);
  return out;
}

// ---- GitHub-flavoured Markdown ----

test('headings, emphasis and inline code render', () => {
  const html = renderToHtml('# Title\n\nSome *em* and **strong** and `code`.');
  assert.match(html, /<h1>Title<\/h1>/);
  assert.match(html, /<em>em<\/em>/);
  assert.match(html, /<strong>strong<\/strong>/);
  assert.match(html, /<code>code<\/code>/);
});

test('tables render inside a scrolling wrapper', () => {
  const html = renderToHtml('| a | b |\n|---|:-:|\n| 1 | 2 |\n');
  assert.match(html, /<div class="md-table-wrap"><table>/);
  assert.match(html, /<th>a<\/th>/);
  assert.match(html, /<td[^>]*>2<\/td>/);
});

test('task lists render disabled checkboxes', () => {
  const html = renderToHtml('- [x] done\n- [ ] todo\n');
  assert.match(html, /<input[^>]*checked[^>]*disabled|<input[^>]*disabled[^>]*checked/);
  assert.equal((html.match(/type="checkbox"/g) ?? []).length, 2);
});

test('fenced code is highlighted with hljs spans, unknown languages stay plain', () => {
  const js = renderToHtml('```js\nconst x = 1;\n```');
  assert.match(js, /<span class="hljs-keyword">const<\/span>/);
  assert.match(js, /data-lang="js"/);
  const unknown = renderToHtml('```nosuchlang\n<b>x</b>\n```');
  assert.match(unknown, /&lt;b&gt;x&lt;\/b&gt;/);
  assert.ok(!unknown.includes('hljs-'));
});

test('highlighting escapes the code it wraps', () => {
  const html = renderToHtml('```html\n<script>alert(1)</script>\n```');
  assert.ok(!html.includes('<script>'));
  assert.match(html, /&lt;/);
});

// ---- Mermaid ----

test('mermaid fence is a diagram only when interactive', () => {
  const src = '```mermaid\ngraph TD; A-->B;\n```';
  assert.match(renderToHtml(src, { interactive: true }), /data-md-mermaid/);
  assert.ok(!renderToHtml(src).includes('data-md-mermaid'));
  assert.ok(!renderToHtml(src, { interactive: true, nested: true }).includes('data-md-mermaid'));
});

test('while streaming, an unclosed mermaid fence stays a code block', () => {
  const open = '```mermaid\ngraph TD; A-->B;';
  assert.ok(!renderToHtml(open, { interactive: true, streaming: true }).includes('data-md-mermaid'));
  // Once the fence closes, the diagram appears.
  assert.match(renderToHtml(`${open}\n\`\`\``, { interactive: true, streaming: true }), /data-md-mermaid/);
  // Not streaming: the document is complete, so an unterminated fence is closed by the end.
  assert.match(renderToHtml(open, { interactive: true }), /data-md-mermaid/);
});

test('a longer closing tilde fence closes a mermaid fence; a shorter one does not', () => {
  const body = '~~~~mermaid\ngraph TD; A-->B;\n';
  const opts = { interactive: true, streaming: true };
  assert.match(renderToHtml(`${body}~~~~~`, opts), /data-md-mermaid/);
  assert.ok(!renderToHtml(`${body}~~~`, opts).includes('data-md-mermaid'));
});

// ---- Frontmatter ----

test('frontmatter becomes a collapsed properties block, not raw text', () => {
  const html = renderToHtml('---\ntitle: My note\ntags: [a, b]\naliases:\n  - one\n  - two\n---\n\nBody text.');
  assert.match(html, /^<details class="md-properties">/);
  assert.ok(!/<details class="md-properties" open/.test(html));
  assert.match(html, /<dt>title<\/dt><dd>My note<\/dd>/);
  assert.match(html, /<dt>tags<\/dt><dd><ul class="md-prop-list"><li>a<\/li><li>b<\/li><\/ul><\/dd>/);
  assert.match(html, /<dt>aliases<\/dt><dd><ul class="md-prop-list"><li>one<\/li><li>two<\/li><\/ul><\/dd>/);
  assert.match(html, /<p>Body text\.<\/p>/);
  assert.ok(!html.includes('title: My note'));
  assert.ok(!html.includes('<hr'));
});

test('frontmatter values can hold wikilinks and are escaped', () => {
  const html = renderToHtml('---\nrelated: "[[Other note]]"\nx: <img src=x onerror=alert(1)>\n---\n');
  assert.match(html, /data-md-kind="vault" data-md-note="Other note"/);
  assert.ok(!html.includes('<img'));
});

test('unclosed frontmatter is ordinary Markdown', () => {
  const html = renderToHtml('---\ntitle: nope\n\nJust text.');
  assert.ok(!html.includes('md-properties'));
  assert.match(html, /<hr>/);
  assert.match(html, /Just text\./);
});

test('while streaming, unclosed frontmatter-looking text shows as growing properties', () => {
  const html = renderToHtml('---\ntitle: Half\ntags:', { streaming: true });
  assert.match(html, /md-properties/);
  assert.match(html, /<dt>title<\/dt><dd>Half<\/dd>/);
  // Prose after the opener means it was a rule, not frontmatter.
  const prose = renderToHtml('---\nThis is a sentence without a colon.', { streaming: true });
  assert.ok(!prose.includes('md-properties'));
});

test('frontmatter only counts at the very start and with CRLF line endings', () => {
  assert.ok(!renderToHtml('Intro\n\n---\na: b\n---\n').includes('md-properties'));
  assert.match(renderToHtml('---\r\na: b\r\n---\r\nBody'), /<dt>a<\/dt><dd>b<\/dd>/);
});

test('nested (embedded) notes drop their frontmatter', () => {
  const html = renderToHtml('---\na: b\n---\nBody', { nested: true });
  assert.ok(!html.includes('md-properties'));
  assert.match(html, /Body/);
});

// ---- Wikilinks ----

test('[[note]] becomes a vault link carrying its target', () => {
  const html = renderToHtml('See [[My Note]] here.');
  assert.match(html, /<a class="md-link md-link-vault" href="#"/);
  assert.match(html, /data-md-kind="vault"/);
  assert.match(html, /data-md-note="My Note"/);
  assert.match(html, />My Note<\/a>/);
});

test('[[note|alias]] shows the alias and keeps both', () => {
  const html = renderToHtml('[[Projects/Plan|the plan]]');
  assert.match(html, /data-md-note="Projects\/Plan"/);
  assert.match(html, /data-md-alias="the plan"/);
  assert.match(html, />the plan<\/a>/);
});

test('heading and block targets are split out', () => {
  const heading = renderToHtml('[[Note#Part two]]');
  assert.match(heading, /data-md-note="Note"/);
  assert.match(heading, /data-md-heading="Part two"/);
  assert.ok(!heading.includes('data-md-block'));
  const block = renderToHtml('[[Note#^abc123|ref]]');
  assert.match(block, /data-md-block="abc123"/);
  assert.ok(!block.includes('data-md-heading'));
  const same = renderToHtml('[[#Local heading]]');
  assert.match(same, /data-md-note=""/);
  assert.match(same, /data-md-heading="Local heading"/);
});

test('an escaped pipe inside a table cell keeps the alias', () => {
  const html = renderToHtml('| a |\n|---|\n| [[Note\\|alias]] |\n');
  assert.match(html, /data-md-note="Note"/);
  assert.match(html, /data-md-alias="alias"/);
});

test('a wikilink inside code is not a link', () => {
  const inline = renderToHtml('Use `[[not a link]]` literally.');
  assert.ok(!inline.includes('data-md-kind'));
  assert.match(inline, /<code>\[\[not a link\]\]<\/code>/);
  const fenced = renderToHtml('```\n[[also not]]\n```');
  assert.ok(!fenced.includes('data-md-kind'));
});

test('empty and malformed wikilinks stay text', () => {
  for (const src of ['[[]]', '[[ ]]', '[[unclosed', '[[a\nb]]']) {
    assert.ok(!renderToHtml(src).includes('data-md-kind'), src);
  }
});

test('wikilink text is escaped', () => {
  const html = renderToHtml('[[<img src=x onerror=alert(1)>]]');
  const els = markup(html);
  assert.ok(!els.some((e) => e.tag === 'img'));
  assert.ok(!els.some((e) => e.attrs.some(([n]) => n.startsWith('on'))));
});

// ---- Markdown links to notes and files (contract) ----

test('relative .md links and obsidian:// links are vault targets', () => {
  const rel = renderToHtml('[x](Folder/My%20Note.md#Heading)');
  assert.match(rel, /data-md-kind="vault"/);
  assert.match(rel, /data-md-note="Folder\/My Note"/);
  assert.match(rel, /data-md-heading="Heading"/);
  const uri = renderToHtml('[x](obsidian://open?vault=V&file=Folder%2FNote)');
  assert.match(uri, /data-md-note="Folder\/Note"/);
});

test('http, mailto, file and plain path links are url targets with an href', () => {
  for (const href of ['https://example.com/a?b=1', 'mailto:a@b.co', 'file:///C:/x/y.txt', 'src/main.rs']) {
    const html = renderToHtml(`[x](${href})`);
    assert.match(html, /data-md-kind="url"/, href);
    assert.ok(html.includes(`href="${href.replace(/&/g, '&amp;')}"`), href);
  }
});

test('bare URLs autolink', () => {
  assert.match(renderToHtml('go to https://example.com now'), /data-md-kind="url"/);
});

// ---- Callouts ----

test('a callout renders with its type, title and body', () => {
  const html = renderToHtml('> [!note] Heads up\n> The body.\n');
  assert.match(html, /<div class="md-callout md-callout-note" data-callout="note">/);
  assert.match(html, /<div class="md-callout-title">Heads up<\/div>/);
  assert.match(html, /<div class="md-callout-content"><p>The body\.<\/p>/);
  assert.ok(!html.includes('[!note]'));
  assert.ok(!html.includes('<blockquote'));
});

test('fold markers: + is open, - is closed', () => {
  const closed = renderToHtml('> [!warning]- Careful\n> text\n');
  assert.match(closed, /<details class="md-callout md-callout-warning" data-callout="warning">/);
  assert.match(closed, /<summary class="md-callout-title">Careful<\/summary>/);
  const open = renderToHtml('> [!tip]+ Handy\n> text\n');
  assert.match(open, /<details class="md-callout md-callout-tip" data-callout="tip" open>/);
});

test('a callout without a title uses the capitalised type', () => {
  const html = renderToHtml('> [!danger]\n> body\n');
  assert.match(html, /<div class="md-callout-title">Danger<\/div>/);
  const noBody = renderToHtml('> [!info]');
  assert.match(noBody, /<div class="md-callout-title">Info<\/div>/);
  assert.ok(!noBody.includes('md-callout-content'));
});

test('aliases fold onto the standard type, unknown types look like a note', () => {
  assert.match(renderToHtml('> [!caution] x'), /md-callout-warning" data-callout="caution"/);
  assert.match(renderToHtml('> [!TLDR] x'), /md-callout-abstract" data-callout="tldr"/);
  assert.match(renderToHtml('> [!my-custom] x'), /md-callout-note" data-callout="my-custom"/);
});

test('callouts nest and hold other blocks', () => {
  const html = renderToHtml('> [!note] Outer\n> text\n>\n> > [!bug] Inner\n> > deep\n>\n> - item\n');
  assert.match(html, /md-callout-note/);
  assert.match(html, /md-callout-bug/);
  assert.match(html, /<li>item<\/li>/);
  assert.ok(html.indexOf('md-callout-note') < html.indexOf('md-callout-bug'));
});

test('callout titles take inline Markdown and wikilinks', () => {
  const html = renderToHtml('> [!note] See [[Other]] for **more**\n> body');
  assert.match(html, /data-md-note="Other"/);
  assert.match(html, /<strong>more<\/strong>/);
});

test('an ordinary blockquote is untouched', () => {
  const html = renderToHtml('> just a quote\n> [!not-first]');
  assert.match(html, /<blockquote>/);
  assert.ok(!html.includes('md-callout'));
});

// ---- Embeds ----

test('![[note]] becomes an embed card with a title link and an empty body', () => {
  const html = renderToHtml('![[Some Note#Part]]');
  assert.match(html, /<span class="md-embed md-embed-note" data-md-embed="note"/);
  assert.match(html, /data-md-note="Some Note"/);
  assert.match(html, /data-md-heading="Part"/);
  assert.match(html, /<a class="md-link md-link-vault md-embed-title"/);
  assert.match(html, /<span class="md-embed-body" hidden><\/span>/);
});

test('![[image.png]] is an image embed and carries the size alias', () => {
  const html = renderToHtml('![[diagram.PNG|300]]');
  assert.match(html, /md-embed-image" data-md-embed="image"/);
  assert.match(html, /data-md-note="diagram.PNG"/);
  assert.match(html, /data-md-alias="300"/);
  assert.ok(!html.includes('<img'));
});

test('embeds inside an embedded note degrade to plain links', () => {
  const html = renderToHtml('![[Deeper]]', { nested: true });
  assert.ok(!html.includes('data-md-embed'));
  assert.match(html, /md-embed-link/);
});

test('an embed in code is not an embed', () => {
  assert.ok(!renderToHtml('`![[x]]`').includes('md-embed'));
});

// ---- Safety ----

const ATTACKS = [
  '<script>alert(1)</script>',
  'text <script>alert(1)</script> text',
  '<img src=x onerror=alert(1)>',
  '<a href="javascript:alert(1)" onclick="x()">x</a>',
  '<svg onload=alert(1)>',
  '<iframe src="https://evil.example"></iframe>',
  '<style>body{display:none}</style>',
  '<div onmouseover="x()">hover</div>\n\nafter',
  '[x](javascript:alert(1))',
  '[x](  JaVaScRiPt:alert(1))',
  '[x](java\tscript:alert(1))',
  '[x](&#106;avascript:alert(1))',
  '[x](vbscript:msgbox(1))',
  '[x](data:text/html;base64,PHNjcmlwdD4=)',
  '[x](//evil.example/a)',
  '![x](javascript:alert(1))',
  '![x](data:image/svg+xml;base64,PHN2Zz4=)',
  '[x]("onmouseover="alert(1))',
  '[[a" onmouseover="alert(1)]]',
  '[t](https://a.example/" onmouseover="alert(1))',
  '> [!note" onclick="x()] t',
  '```"><script>alert(1)</script>\ncode\n```',
  '---\n"><script>alert(1)</script>: v\n---\n',
];

const ALLOWED_TAGS = new Set([
  'p', 'br', 'a', 'em', 'strong', 'del', 'code', 'pre', 'span', 'div', 'ul', 'ol', 'li', 'blockquote',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td', 'details',
  'summary', 'dl', 'dt', 'dd', 'input', 'img',
]);

test('no attack vector yields an unlisted element, an event handler or a live script URL', () => {
  for (const src of ATTACKS) {
    for (const opts of [{}, { interactive: true, streaming: true }]) {
      const html = renderToHtml(src, opts);
      for (const el of markup(html)) {
        assert.ok(ALLOWED_TAGS.has(el.tag), `<${el.tag}> from: ${src}`);
        for (const [name, value] of el.attrs) {
          assert.ok(!name.startsWith('on'), `${name} from: ${src}`);
          assert.ok(name !== 'style' && name !== 'srcdoc' && name !== 'formaction', `${name} from: ${src}`);
          if (name === 'href' || name === 'src') {
            assert.ok(!/^\s*(javascript|vbscript|data:text)/i.test(value), `${name}=${value} from: ${src}`);
          }
        }
      }
    }
  }
});

test('raw HTML shows as an inert, escaped marker', () => {
  const block = renderToHtml('<div onclick="x()">hi</div>');
  assert.match(block, /<pre class="md-raw-html"><code>&lt;div onclick=&quot;x\(\)&quot;&gt;hi&lt;\/div&gt;<\/code><\/pre>/);
  const inline = renderToHtml('a <b>bold</b> b');
  assert.match(inline, /<code class="md-raw-html">&lt;b&gt;<\/code>bold/);
});

test('<br> survives and HTML comments vanish', () => {
  assert.match(renderToHtml('a<br>b'), /a<br>b/);
  assert.ok(!renderToHtml('x\n\n<!-- hidden -->\n\ny').includes('hidden'));
});

test('a dangerous link keeps its text but loses the anchor', () => {
  const html = renderToHtml('[click me](javascript:alert(1))');
  assert.ok(!html.includes('<a'));
  assert.match(html, /click me/);
});

test('a remote image is never emitted as an <img>; it becomes a link', () => {
  const html = renderToHtml('![a cat](https://tracker.example/pixel.png)');
  assert.ok(!html.includes('<img'));
  assert.match(html, /<a class="md-link md-link-url md-image-link" href="https:\/\/tracker\.example\/pixel\.png"/);
  assert.match(html, />a cat<\/a>/);
  // Relative images are not fetched either. A file on this computer is an <img> with no src at all
  // (the host sends the picture, see localImages.ts), so the browser fetches nothing.
  assert.ok(!renderToHtml('![x](pics/a.png)').includes('<img'));
  assert.ok(!renderToHtml('![x](file:///C:/a.png)').includes(' src='));
});

test('an inline base64 image is the only image that renders', () => {
  const ok = renderToHtml('![dot](data:image/png;base64,iVBORw0KGgo=)');
  assert.match(ok, /<img class="md-image" src="data:image\/png;base64,iVBORw0KGgo="/);
  assert.ok(!renderToHtml('![x](data:image/svg+xml;base64,PHN2Zz4=)').includes('<img'));
});

test('rendering never throws, even on hostile input', () => {
  const deep = '> '.repeat(300) + 'x';
  const html = renderToHtml(deep);
  assert.equal(typeof html, 'string');
  assert.equal(typeof renderToHtml('[[' + 'a'.repeat(100_000) + ']]'), 'string');
});

// ---- Clickable paths ----

test('inline code holding a whole path becomes a path link around the code', () => {
  const html = renderToHtml('Edit `src/app.ts:12` then run `npm test`.');
  assert.match(html, /<a class="md-link md-link-path md-path" href="#"[^>]*data-md-kind="path" data-md-href="src\/app.ts:12"><code>src\/app.ts:12<\/code><\/a>/);
  assert.match(html, /<code>npm test<\/code>/);
  assert.ok(!/data-md-href="npm test"/.test(html));
});

test('plain-text paths become path links; URLs, fractions and dates do not', () => {
  const html = renderToHtml('See ~/notes/plan.md and (./build.sh), or src/a/b.ts:3. Not 1/2, 2026/10/17, and/or https://x.example/a/b.ts.');
  assert.match(html, /data-md-href="~\/notes\/plan.md"/);
  assert.match(html, /\(<a [^>]*data-md-href=".\/build.sh"/);
  assert.match(html, /data-md-href="src\/a\/b.ts:3">src\/a\/b.ts:3<\/a>\./, 'the sentence dot stays outside');
  for (const not of ['1/2', '2026/10/17', 'and/or']) assert.ok(!html.includes(`data-md-href="${not}"`), not);
  assert.match(html, /<a class="md-link md-link-url" href="https:\/\/x.example\/a\/b.ts"/);
  assert.equal((html.match(/md-link-path/g) ?? []).length, 3);
});

test('a path inside link text is not linked twice', () => {
  const html = renderToHtml('[open src/a/b.ts](https://x.example)');
  assert.equal((html.match(/<a /g) ?? []).length, 1);
});

test('a diagram is named from the first word of its source, comments and blank lines skipped', async () => {
  const { diagramKind } = await import('./mermaid.ts');
  const cases: [string, string][] = [
    ['flowchart LR\n  A --> B', 'Flowchart'],
    ['\n%% note\ngraph TD\n  A-->B', 'Flowchart'],
    ['sequenceDiagram\n  A->>B: hi', 'Sequence diagram'],
    ['stateDiagram-v2\n  [*] --> A', 'State diagram'],
    ['quadrantChart\n  title x', 'Diagram'],
    ['', 'Diagram'],
  ];
  for (const [source, kind] of cases) assert.equal(diagramKind(source), kind, source);
});

test('math: $x$ and $$ blocks are marked for typesetting, prices and code are left alone', () => {
  const marked = (md: string) => [...renderToHtml(md).matchAll(/data-md-math="(\w+)"><span class="md-math-src">([^<]*)</g)].map((m) => `${m[1]}:${m[2]}`);
  const cases: [string, string[]][] = [
    ['Energy $E = mc^2$ here.', ['inline:E = mc^2']],
    ['Two $a$ and $b$ in a line', ['inline:a', 'inline:b']],
    ['It costs $5 and $10 in total', []],
    ['A lone $ sign, or $ spaced $ ones', []],
    ['Use `$x$` in code', []],
    ['Escaped \\$x\\$ stays', []],
    ['$$\\int_0^1 x\\,dx$$ inline display', ['display:\\int_0^1 x\\,dx']],
    ['Before\n\n$$\na^2 + b^2 = c^2\n$$\n\nAfter', ['display:a^2 + b^2 = c^2']],
    ['$$\nunfinished', []],
    ['```\n$$\nx\n$$\n```', []],
  ];
  for (const [src, want] of cases) assert.deepEqual(marked(src), want, src);
});

test('a local picture is marked with its path and drawn later; other image targets are as before', async () => {
  const { localImagePath } = await import('./localImages.ts');
  const cases: [string, string | null][] = [
    ['C:\\Users\\me\\shot.png', 'C:\\Users\\me\\shot.png'],
    ['C:/Users/me/shot.JPG', 'C:/Users/me/shot.JPG'],
    ['/home/me/a.webp', '/home/me/a.webp'],
    ['file:///C:/Users/me/a%20b.gif', 'C:/Users/me/a b.gif'],
    ['file:///home/me/a.png', '/home/me/a.png'],
    ['//server/share/a.png', null],
    ['relative/a.png', null],
    ['C:\\Users\\me\\notes.txt', null],
    ['C:\\Users\\me\\vector.svg', null],
    ['https://example.com/a.png', null],
    ['data:image/png;base64,AAAA', null],
  ];
  for (const [href, want] of cases) assert.equal(localImagePath(href), want, href);
  const html = renderToHtml('![the shot](C:/Users/me/shot.png)');
  assert.match(html, /<img class="md-image md-image-local" data-md-local-src="C:\/Users\/me\/shot\.png" alt="the shot">/);
  assert.ok(!html.includes(' src='), 'no src until the host answers');
});
