// One terminal session's view: xterm.js with fit, search, Unicode 11, web links and WebGL (falling
// back to the DOM renderer on context loss). Output arrives as `terminal` events; writes are acked
// in batches so the host's 1 MiB pause never trips under normal use.

import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { WebglAddon } from '@xterm/addon-webgl';
import { Terminal, type ITheme } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { forwardRef, useEffect, useImperativeHandle, useRef } from 'react';
import type { EventBus } from '../api/events.ts';
import type { TerminalOps } from '../api/terminal.ts';
import { AckBatcher, PALETTE, routeKey, SeqFilter } from './model.ts';

export interface XtermHandle {
  find(q: string, back?: boolean): boolean;
  clearFind(): void;
  focus(): void;
  clear(): void;
  fit(): void;
}

function themeFor(): ITheme {
  const css = getComputedStyle(document.documentElement);
  const v = (n: string) => css.getPropertyValue(n).trim();
  const dark = document.documentElement.dataset.theme === 'dark';
  const p = dark ? PALETTE.dark : PALETTE.light;
  const names = ['black', 'red', 'green', 'yellow', 'blue', 'magenta', 'cyan', 'white'] as const;
  const t: Record<string, string> = {
    background: v('--surface') || (dark ? '#1b1b1b' : '#fefefe'),
    foreground: v('--text') || (dark ? '#ffffff' : '#262826'),
    cursor: v('--accent'),
    cursorAccent: v('--surface'),
    selectionBackground: `color-mix(in srgb, ${v('--accent')} 34%, transparent)`,
  };
  names.forEach((n, i) => {
    t[n] = p.normal[i] ?? '';
    t[`bright${n[0]?.toUpperCase()}${n.slice(1)}`] = p.bright[i] ?? '';
  });
  return t as ITheme;
}

export interface XtermViewProps {
  id: string;
  ops: TerminalOps;
  bus: EventBus;
  visible: boolean;
  onOutput: () => void;
  onExit: (code: number) => void;
  onCommand: (cmd: string) => void;
  openLink: (url: string) => void;
}

export const XtermView = forwardRef<XtermHandle, XtermViewProps>(function XtermView({ id, ops, bus, visible, onOutput, onExit, onCommand, openLink }, ref) {
  const host = useRef<HTMLDivElement>(null);
  const term = useRef<Terminal | null>(null);
  const fit = useRef<FitAddon | null>(null);
  const search = useRef<SearchAddon | null>(null);
  const exited = useRef(false);
  const cb = useRef({ onOutput, onExit, onCommand, openLink });
  cb.current = { onOutput, onExit, onCommand, openLink };

  useImperativeHandle(ref, () => ({
    find: (q, back) => (q ? (back ? search.current?.findPrevious(q) : search.current?.findNext(q)) ?? false : false),
    clearFind: () => search.current?.clearDecorations(),
    focus: () => term.current?.focus(),
    clear: () => term.current?.clear(),
    fit: () => fit.current?.fit(),
  }));

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const css = getComputedStyle(document.documentElement);
    const t = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      cursorStyle: 'block',
      scrollback: 10_000,
      fontFamily: css.getPropertyValue('--font-mono').trim() || 'Consolas, monospace',
      fontSize: 12.5,
      lineHeight: 1.18,
      theme: themeFor(),
    });
    const f = new FitAddon();
    const s = new SearchAddon();
    t.loadAddon(f);
    t.loadAddon(s);
    t.loadAddon(new Unicode11Addon());
    t.unicode.activeVersion = '11';
    t.loadAddon(new WebLinksAddon((_e, uri) => cb.current.openLink(uri)));
    t.open(el);
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose());
      t.loadAddon(gl);
    } catch {
      // No WebGL: the DOM renderer stays.
    }
    term.current = t;
    fit.current = f;
    search.current = s;
    t.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true;
      const r = routeKey(e);
      if (r === 'shell') return true;
      if (r === 'app') return false; // let the window's key registry see it
      e.preventDefault();
      if (r === 'copy') {
        const sel = t.getSelection();
        if (sel) void navigator.clipboard?.writeText(sel);
      } else if (r === 'paste') {
        void navigator.clipboard?.readText().then((txt) => txt && t.paste(txt));
      } else if (r === 'clear') {
        t.clear();
      } else {
        cb.current.onCommand(r);
      }
      return false;
    });
    // The last few lines as an attribute, so DOM checks can read a WebGL-drawn terminal.
    const publishTail = () => {
      const b = t.buffer.active;
      const lines: string[] = [];
      for (let y = Math.max(0, b.length - 30); y < b.length; y++) {
        const l = b.getLine(y)?.translateToString(true);
        if (l) lines.push(l);
      }
      el.dataset.tail = lines.slice(-12).join('\n');
    };
    const acks = new AckBatcher((units) => void ops.ack(id, units).catch(() => undefined));
    const seq = new SeqFilter();
    let raf = 0;
    const offOut = bus.on('terminal', (ev) => {
      if (ev.id !== id || typeof ev.data !== 'string' || typeof ev.seq !== 'number') return;
      if (!seq.accept(ev.seq)) return;
      const data = ev.data;
      t.write(data, () => {
        acks.wrote(data.length);
        cancelAnimationFrame(raf);
        raf = requestAnimationFrame(() => {
          acks.flush();
          publishTail();
        });
      });
      cb.current.onOutput();
    });
    const offExit = bus.on('terminal_exit', (ev) => {
      if (ev.id !== id) return;
      exited.current = true;
      const code = typeof ev.code === 'number' ? ev.code : -1;
      t.write(`\r\n\x1b[2mprocess exited with code ${code}; press any key to close\x1b[0m\r\n`);
      cb.current.onExit(code);
    });
    const onData = t.onData((d) => {
      if (exited.current) {
        cb.current.onCommand('close');
        return;
      }
      void ops.write(id, d).catch(() => undefined);
    });
    const onResize = t.onResize(({ cols, rows }) => void ops.resize(id, cols, rows).catch(() => undefined));
    // Reattach: replay recent output, then accept only newer chunks.
    void ops
      .attach(id)
      .then((r) => {
        seq.reset(r.seq);
        if (r.replay) t.write(r.replay);
        if (!r.session.running) exited.current = true;
      })
      .catch(() => undefined);
    const ro = new ResizeObserver(() => {
      if (el.offsetWidth > 0 && el.offsetHeight > 0) {
        try {
          f.fit();
        } catch {
          // Not laid out yet.
        }
      }
    });
    ro.observe(el);
    // The theme follows colour-mode changes.
    const mo = new MutationObserver(() => (t.options.theme = themeFor()));
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
    return () => {
      offOut();
      offExit();
      onData.dispose();
      onResize.dispose();
      ro.disconnect();
      mo.disconnect();
      cancelAnimationFrame(raf);
      acks.flush();
      t.dispose();
      term.current = null;
    };
  }, [id, ops, bus]);

  useEffect(() => {
    if (visible) {
      requestAnimationFrame(() => {
        try {
          fit.current?.fit();
        } catch {
          // Not laid out yet.
        }
        term.current?.focus();
      });
    }
  }, [visible]);

  return <div ref={host} className="term-view" data-testid="terminal-session" data-session={id} hidden={!visible} />;
});
