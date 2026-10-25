// Visual sweep of the real debug exe: DOM measurements only (computed styles, boxes and
// attributes), never screenshots read back. Every surface is visited once and measured in each
// colour mode (light, dark) under each media condition (normal, forced colours, increased
// contrast), by emulating the media feature on the page, so the state is built once per surface.
//
// What it asserts, per surface and mode:
//   - text contrast: every visible text run reaches 4.5:1 (3:1 for large text) against the colours
//     painted behind it, except runs on the short allow-list below (a documented design-system value);
//   - focus: every keyboard stop shows a ring (an outline, or a border/box-shadow that differs from
//     its unfocused look), and no ring is cut off by an overflow clip;
//   - forced colours: no control is left without a border, outline or text, and a selected, pressed
//     or current state stays distinguishable from its unselected peer without colour;
//   - Reduce Motion: no animation or transition runs.
// Toasts are asserted separately: text >= 4.5:1, icons >= 3:1, in light and dark.
//
// HELM_W5V_REPORT=<file> writes everything measured (including what passed) as JSON.
//
// Run: `npm run build:debug` at the root, then `npm run e2e -w checks`.

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { after, before, describe, test } from "node:test";
import type { Page } from "playwright-core";
import { api, dispose, launchApp, newChat, scratchDir, send, shot, tid, waitStatus, type App } from "./harness.ts";

const OUT = process.env.HELM_E2E_OUT ?? scratchDir("helm-e2e-out-");
const profile = scratchDir("helm-w5v-profile-");
const REPORT = process.env.HELM_W5V_REPORT;
let app: App | undefined;
const page = () => app!.page;

type Theme = "light" | "dark";
type Media = "normal" | "forced" | "more";
const QUICK = process.env.HELM_W5V_QUICK === "1";
const THEMES: Theme[] = QUICK ? ["light"] : ["light", "dark"];
const MEDIA: Media[] = QUICK ? ["normal"] : ["normal", "forced", "more"];

interface Finding {
  surface: string;
  theme: Theme;
  media: Media;
  kind: "contrast" | "focus" | "forced" | "motion";
  what: string;
}
const findings: Finding[] = [];
// Findings that are a design decision still to be taken, each named by what it is (`item`).
// Only these exact controls are excused; any other finding still fails the report. Delete an entry
// when its decision is made.
const KNOWN: { kind: Finding["kind"]; what: RegExp; item: string }[] = [
  // Muted colours below 4.5:1: the sidebar's shortcut hint and Settings → Actions' muted text.
  { kind: "contrast", what: /^kbd\.sb-kbd /, item: "sidebar shortcut hint colour" },
  { kind: "contrast", what: /^span\.ap-(label|tag|preview) /, item: "Actions muted text colour" },
  // The composer shows focus as its box's border tint only (composer.css), a deliberate choice.
  { kind: "focus", what: /^div\[composer\]\{Message\}\.editor-input: no outline or shadow when focused/, item: "composer focus tint" },
  // The search dialog and quick-open fields have no box of their own: focus shows as a line under
  // the field (the search dialog), and the dialog's edge frames them.
  { kind: "focus", what: /^input\[search-input\]\{Search\}\.search-input: no outline or shadow when focused/, item: "search field focus line" },
  { kind: "forced", what: /^field input(\[search-input\])?\.(search|qo)-input has no border/, item: "borderless dialog fields" },
];
const measured: Record<string, unknown> = {};
// The same defect on the same control is one finding, however many surfaces show it.
const seenFindings = new Set<string>();
function add(f: Finding): void {
  const key = `${f.theme}/${f.media}/${f.kind}/${f.what.replace(/ ring\[[^\]]*\] clip\[[^\]]*\]/, "")}`;
  if (seenFindings.has(key)) return;
  seenFindings.add(key);
  findings.push(f);
}

async function setLook(theme: Theme, media: Media, reduceMotion = false): Promise<void> {
  await api(page(), "preference", { key: "appearance", value: theme });
  await page().waitForFunction((m) => document.documentElement.dataset.theme === m, theme, { timeout: 5_000 });
  await page().emulateMedia({
    colorScheme: theme,
    forcedColors: media === "forced" ? "active" : "none",
    contrast: media === "more" ? "more" : "no-preference",
    reducedMotion: reduceMotion ? "reduce" : "no-preference",
  });
  await page().waitForTimeout(media === "normal" && !reduceMotion ? 450 : 150);
}

// ---- in-page audits --------------------------------------------------------------------------

/** Contrast of every visible text run against what is painted behind it. */
const AUDIT_TEXT = ([skip, rootSel]: [string[], string | null]) => {
  const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true })!;
  const parse = (c: string): [number, number, number, number] => {
    probe.clearRect(0, 0, 1, 1);
    probe.fillStyle = "#000";
    probe.fillStyle = c;
    probe.fillRect(0, 0, 1, 1);
    const d = probe.getImageData(0, 0, 1, 1).data;
    return [d[0]!, d[1]!, d[2]!, d[3]! / 255];
  };
  const lum = (r: number, g: number, b: number) => {
    const f = (x: number) => ((x /= 255) <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  };
  const name = (el: Element): string => {
    const t = el.getAttribute("data-testid");
    const c = (el.getAttribute("class") ?? "").split(/\s+/).filter(Boolean).slice(0, 2).join(".");
    return `${el.tagName.toLowerCase()}${t ? `[${t}]` : ""}${c ? "." + c : ""}`;
  };
  const out: { sel: string; text: string; ratio: number; size: number; fg: string; bg: string; uncertain: boolean }[] = [];
  const seen = new Set<string>();
  const walker = document.createTreeWalker(rootSel ? document.querySelector(rootSel)! : document.body, NodeFilter.SHOW_TEXT);
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const text = (n.textContent ?? "").trim();
    if (!text) continue;
    const el = n.parentElement;
    if (!el || ["SCRIPT", "STYLE", "OPTION"].includes(el.tagName)) continue;
    if (skip.includes("sidebar") && el.closest(".shell-sidebar")) continue;
    if (skip.includes("topbar") && el.closest('[data-testid="topbar"]')) continue;
    const r = document.createRange();
    r.selectNodeContents(n);
    const rect = r.getBoundingClientRect();
    if (rect.width < 2 || rect.height < 2 || rect.bottom < 0 || rect.right < 0 || rect.top > innerHeight || rect.left > innerWidth) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility !== "visible" || cs.display === "none") continue;
    // Gradient-filled text (the dark wordmark: `background-clip: text`, `color: transparent`) paints
    // its fill from the background image, so `color` says nothing about its contrast.
    if (cs.backgroundClip === "text" || cs.getPropertyValue("-webkit-background-clip") === "text") continue;
    // A disabled control is exempt (WCAG 1.4.3); so is text nobody can see (clipped to nothing).
    if (el.closest(":disabled, [aria-disabled='true'], [hidden], .visually-hidden, [aria-hidden='true']")) continue;
    // Layers from the element up to the root, with the opacity they apply.
    let opacity = 1;
    let uncertain = false;
    const layers: [number, number, number, number][] = [];
    for (let a: Element | null = el; a; a = a.parentElement) {
      const s = getComputedStyle(a);
      opacity *= Number(s.opacity);
      const bg = parse(s.backgroundColor);
      if (bg[3] > 0) layers.push(bg);
      if (s.backgroundImage !== "none") uncertain = true;
      if (bg[3] >= 0.999) break;
    }
    if (opacity < 0.05) continue;
    // Composite from the bottom layer up; below all of them, the page.
    let base: [number, number, number] = parse(getComputedStyle(document.documentElement).backgroundColor).slice(0, 3) as [number, number, number];
    for (const l of layers.reverse()) base = [0, 1, 2].map((i) => l[i]! * l[3] + base[i]! * (1 - l[3])) as [number, number, number];
    const fgp = parse(cs.color);
    const a = fgp[3] * opacity;
    const fg = [0, 1, 2].map((i) => fgp[i]! * a + base[i]! * (1 - a)) as [number, number, number];
    const l1 = lum(...fg);
    const l2 = lum(...base);
    const ratio = (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
    const size = parseFloat(cs.fontSize);
    const key = `${name(el)}|${ratio.toFixed(2)}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ sel: name(el), text: text.slice(0, 40), ratio: Math.round(ratio * 100) / 100, size, fg: `rgb(${fg.map(Math.round)})`, bg: `rgb(${base.map(Math.round)})`, uncertain });
  }
  return out;
};

/** Tabs through the page and reports what each keyboard stop looks like focused. */
const AUDIT_FOCUS_STEP = () => {
  const el = document.activeElement as HTMLElement | null;
  if (!el || el === document.body) return null;
  const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true })!;
  const parse = (c: string): [number, number, number, number] => {
    probe.clearRect(0, 0, 1, 1);
    probe.fillStyle = "#000";
    probe.fillStyle = c;
    probe.fillRect(0, 0, 1, 1);
    const d = probe.getImageData(0, 0, 1, 1).data;
    return [d[0]!, d[1]!, d[2]!, d[3]! / 255];
  };
  const lum = (r: number, g: number, b: number) => {
    const f = (x: number) => ((x /= 255) <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  };
  const clipping = (s: CSSStyleDeclaration, axis: "x" | "y") => /(hidden|clip|auto|scroll)/.test(axis === "x" ? s.overflowX : s.overflowY);
  const label = (e: Element) => `${e.tagName.toLowerCase()}${e.getAttribute("data-testid") ? `[${e.getAttribute("data-testid")}]` : ""}${e.getAttribute("aria-label") ? `{${e.getAttribute("aria-label")}}` : ""}${(e.getAttribute("class") ?? "").trim() ? "." + (e.getAttribute("class") ?? "").trim().split(/\s+/).slice(0, 2).join(".") : ""}`;
  const has = (s: CSSStyleDeclaration) => s.outlineStyle !== "none" && parseFloat(s.outlineWidth) > 0;
  let ringEl: HTMLElement = el;
  if (!has(getComputedStyle(el))) {
    // A field whose own outline is off may show its ring on the box that wraps it (:focus-within).
    for (let a = el.parentElement, i = 0; a && i < 4; a = a.parentElement, i++) {
      if (has(getComputedStyle(a))) {
        ringEl = a;
        break;
      }
    }
  }
  const cs = getComputedStyle(ringEl);
  const outlineOn = has(cs);
  const r = ringEl.getBoundingClientRect();
  const off = parseFloat(cs.outlineOffset) || 0;
  const w = outlineOn ? parseFloat(cs.outlineWidth) : 0;
  let clipped = false;
  let clipBy = "";
  if (outlineOn) {
    const ring = { l: r.left - off - w, t: r.top - off - w, r: r.right + off + w, b: r.bottom + off + w };
    for (let a = ringEl.parentElement; a && a !== document.documentElement; a = a.parentElement) {
      const s = getComputedStyle(a);
      const ar = a.getBoundingClientRect();
      // A control scrolled half out of view is cut by the scroller, not by its ring: skip it.
      const inside = r.left >= ar.left - 1 && r.right <= ar.right + 1 && r.top >= ar.top - 1 && r.bottom <= ar.bottom + 1;
      if (!inside) continue;
      const cutX = clipping(s, "x") && (ring.l < ar.left - 0.5 || ring.r > ar.right + 0.5);
      const cutY = clipping(s, "y") && (ring.t < ar.top - 0.5 || ring.b > ar.bottom + 0.5);
      if (cutX || cutY) {
        clipped = true;
        clipBy = `${label(a)} ${cutX ? "x" : ""}${cutY ? "y" : ""} ring[${Math.round(ring.l)},${Math.round(ring.t)},${Math.round(ring.r)},${Math.round(ring.b)}] clip[${Math.round(ar.left)},${Math.round(ar.top)},${Math.round(ar.right)},${Math.round(ar.bottom)}]`;
        break;
      }
    }
  }
  // The ring against what is painted around it (WCAG 1.4.11: 3:1).
  let ringRatio = 0;
  if (outlineOn) {
    const layers: [number, number, number, number][] = [];
    for (let a: Element | null = ringEl.parentElement; a; a = a.parentElement) {
      const b = parse(getComputedStyle(a).backgroundColor);
      if (b[3] > 0) layers.push(b);
      if (b[3] >= 0.999) break;
    }
    let base: [number, number, number] = parse(getComputedStyle(document.documentElement).backgroundColor).slice(0, 3) as [number, number, number];
    for (const l of layers.reverse()) base = [0, 1, 2].map((i) => l[i]! * l[3] + base[i]! * (1 - l[3])) as [number, number, number];
    const o = parse(cs.outlineColor);
    const c = [0, 1, 2].map((i) => o[i]! * o[3] + base[i]! * (1 - o[3])) as [number, number, number];
    const l1 = lum(...c);
    const l2 = lum(...base);
    ringRatio = Math.round(((Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05)) * 100) / 100;
  }
  const own = getComputedStyle(el);
  return {
    name: label(el),
    visible: el.getBoundingClientRect().width > 0 && el.getBoundingClientRect().height > 0,
    outline: outlineOn ? `${cs.outlineWidth} ${cs.outlineStyle} ${cs.outlineColor} +${cs.outlineOffset}` : "none",
    ringRatio,
    boxShadow: own.boxShadow,
    border: `${own.borderTopWidth} ${own.borderTopStyle} ${own.borderTopColor}`,
    bg: own.backgroundColor,
    clipped,
    clipBy,
    region: el.closest(".shell-sidebar") ? "sidebar" : el.closest('[data-testid="topbar"]') ? "topbar" : "",
  };
};


/** Marks small coloured boxes with no text and no icon (dots, swatches) in normal colours. */
const MARK_SWATCHES = () => {
  const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true })!;
  const alpha = (c: string) => {
    probe.clearRect(0, 0, 1, 1);
    probe.fillStyle = "#000";
    probe.fillStyle = c;
    probe.fillRect(0, 0, 1, 1);
    return probe.getImageData(0, 0, 1, 1).data[3]! / 255;
  };
  let n = 0;
  for (const el of document.querySelectorAll<HTMLElement>("body *")) {
    el.removeAttribute("data-w5sw");
    const r = el.getBoundingClientRect();
    if (r.width < 3 || r.height < 3 || r.width > 26 || r.height > 26) continue;
    if (el instanceof HTMLInputElement && ["checkbox", "radio", "range"].includes(el.type)) continue;
    if (el.closest("svg") || el.tagName === "svg" || el.querySelector("svg, img") || (el.textContent ?? "").trim()) continue;
    if (el.children.length > 0) continue;
    const cs = getComputedStyle(el);
    if (cs.visibility !== "visible" || cs.display === "none" || Number(cs.opacity) < 0.2) continue;
    if (alpha(cs.backgroundColor) < 0.5) continue;
    el.setAttribute("data-w5sw", String(n++));
  }
  return n;
};

/** Under forced colours: what is left of a mark, a selected state and a field. */
const AUDIT_FORCED = () => {
  const visible = (e: Element) => {
    const r = e.getBoundingClientRect();
    const cs = getComputedStyle(e);
    return r.width > 0 && r.height > 0 && cs.visibility === "visible" && cs.display !== "none" && !e.closest("[hidden], [aria-hidden='true']");
  };
  const label = (e: Element) => `${e.tagName.toLowerCase()}${e.getAttribute("data-testid") ? `[${e.getAttribute("data-testid")}]` : ""}${(e.getAttribute("class") ?? "").trim() ? "." + (e.getAttribute("class") ?? "").trim().split(/\s+/).slice(0, 2).join(".") : ""}`;
  const border = (cs: CSSStyleDeclaration) => ["Top", "Right", "Bottom", "Left"].some((k) => parseFloat(cs.getPropertyValue(`border-${k.toLowerCase()}-width`)) > 0 && cs.getPropertyValue(`border-${k.toLowerCase()}-style`) !== "none");
  const sig = (e: Element) => {
    const s = getComputedStyle(e);
    return [s.color, s.backgroundColor, s.borderTopColor, s.borderTopWidth, s.borderBottomColor, s.borderBottomWidth, s.borderLeftColor, s.borderLeftWidth, s.outlineStyle, s.outlineWidth, s.outlineColor, s.textDecorationLine, s.fontWeight, s.fontStyle, s.boxShadow === "none" ? "" : s.boxShadow].join("|");
  };
  const out: string[] = [];
  // 1. Marks that were a colour and nothing else.
  for (const el of document.querySelectorAll<HTMLElement>("[data-w5sw]")) {
    if (!visible(el)) continue;
    const cs = getComputedStyle(el);
    if (border(cs) || cs.outlineStyle !== "none" || cs.forcedColorAdjust === "none") continue;
    out.push(`mark ${label(el)} keeps no border or outline (colour was its only shape)`);
  }
  // 2. A selected, pressed or current state against its unselected peers.
  const state = "[aria-selected='true'], [aria-pressed='true'], [aria-current='true'], [aria-current='page'], [aria-checked='true'], [data-selected='true'], [data-active='true']";
  for (const el of document.querySelectorAll<HTMLElement>(state)) {
    if (!visible(el) || !el.parentElement) continue;
    const peers = [...el.parentElement.children].filter((p) => p !== el && p.tagName === el.tagName && !p.matches(state) && visible(p));
    if (peers.length === 0) continue;
    const mine = sig(el);
    if (peers.every((p) => sig(p) === mine)) out.push(`state ${label(el)} looks the same as its unselected peers`);
  }
  // 3. A field with no edge.
  for (const el of document.querySelectorAll<HTMLElement>("input:not([type='checkbox']):not([type='radio']):not([type='range']):not([type='hidden']), textarea, select, [contenteditable='true']")) {
    if (!visible(el) || el.classList.contains("xterm-helper-textarea")) continue;
    const cs = getComputedStyle(el);
    if (!border(cs) && cs.outlineStyle === "none") {
      const box = el.closest(".composer-box, .ui-field, label") as HTMLElement | null;
      if (box && border(getComputedStyle(box))) continue;
      out.push(`field ${label(el)} has no border`);
    }
  }
  return [...new Set(out)];
};

/** The colour of every text run and icon inside `rootSel`, against the toast's own opaque fill. */
const MEASURE_TOASTS = (rootSel: string) => {
  const probe = document.createElement("canvas").getContext("2d", { willReadFrequently: true })!;
  const parse = (c: string): [number, number, number, number] => {
    probe.clearRect(0, 0, 1, 1);
    probe.fillStyle = "#000";
    probe.fillStyle = c;
    probe.fillRect(0, 0, 1, 1);
    const d = probe.getImageData(0, 0, 1, 1).data;
    return [d[0]!, d[1]!, d[2]!, d[3]! / 255];
  };
  const lum = ([r, g, b]: number[]) => {
    const f = (x: number) => ((x /= 255) <= 0.03928 ? x / 12.92 : ((x + 0.055) / 1.055) ** 2.4);
    return 0.2126 * f(r!) + 0.7152 * f(g!) + 0.0722 * f(b!);
  };
  const out: { kind: string; part: "text" | "icon"; text: string; ratio: number }[] = [];
  for (const root of document.querySelectorAll<HTMLElement>(rootSel)) {
    // The toast is opaque: its own fill is what its text sits on.
    const fill = parse(getComputedStyle(root).backgroundColor);
    if (fill[3] < 0.99) out.push({ kind: root.getAttribute("data-kind") ?? "", part: "text", text: "(fill not opaque)", ratio: 1 });
    const bg = fill.slice(0, 3);
    const ratio = (fg: number[]) => {
      const l1 = lum(fg);
      const l2 = lum(bg);
      return Math.round(((Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05)) * 100) / 100;
    };
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const text = (n.textContent ?? "").trim();
      const el = n.parentElement;
      if (!text || !el) continue;
      const c = parse(getComputedStyle(el).color);
      const a = c[3] * Number(getComputedStyle(el).opacity);
      out.push({ kind: root.getAttribute("data-kind") ?? "", part: "text", text: text.slice(0, 24), ratio: ratio([0, 1, 2].map((i) => c[i]! * a + bg[i]! * (1 - a))) });
    }
    for (const svg of root.querySelectorAll("svg")) {
      const cs = getComputedStyle(svg);
      const paint = cs.stroke !== "none" && cs.stroke !== "" ? cs.stroke : cs.fill;
      const c = parse(/currentcolor/i.test(paint) ? cs.color : paint);
      out.push({ kind: root.getAttribute("data-kind") ?? "", part: "icon", text: svg.closest("button")?.getAttribute("aria-label") ?? "icon", ratio: ratio([0, 1, 2].map((i) => c[i]! * c[3] + bg[i]! * (1 - c[3]))) });
    }
  }
  return out;
};

/** Adds one toast of every kind, built like ui/src/ui/misc.tsx's Toast, one under the other. */
const INJECT_TOASTS = () => {
  const svg = (d: string) => `<svg class="ui-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${d}</svg>`;
  const alert = svg('<circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/>');
  const info = svg('<circle cx="12" cy="12" r="10"/><path d="M12 16v-4M12 8h.01"/>');
  const x = svg('<path d="M18 6 6 18M6 6l12 12"/>');
  const box = document.createElement("div");
  box.className = "w5-toasts";
  box.style.cssText = "position:fixed;left:50%;top:120px;transform:translateX(-50%);display:flex;flex-direction:column;gap:6px;z-index:2000";
  for (const [kind, text, action] of [
    ["error", "Could not save the setting.", "Retry"],
    ["info", "Exported 3 entries.", ""],
    ["restored", "Chat restored.", "Open"],
    ["undo", "Chat deleted.", "Undo"],
    ["reorganize", "Moved 2 chats.", "Undo"],
  ] as const) {
    const t = document.createElement("div");
    t.className = "ui-toast";
    t.setAttribute("role", "status");
    t.setAttribute("data-kind", kind);
    t.innerHTML = `<span class="ui-toast-icon">${kind === "error" ? alert : info}</span><span class="ui-toast-text">${text}</span>${action ? `<button type="button" class="ui-btn ui-btn-text ui-toast-action">${action}<kbd>Ctrl+Z</kbd></button>` : ""}<button type="button" class="ui-btn ui-btn-icon ui-toast-x" aria-label="Dismiss notice">${x}</button>`;
    box.appendChild(t);
  }
  document.body.appendChild(box);
};

// ---- helpers -------------------------------------------------------------------------------

async function injectToasts(): Promise<void> {
  await page().evaluate(INJECT_TOASTS);
}
async function dropToasts(): Promise<void> {
  await page().evaluate(() => document.querySelectorAll(".w5-toasts").forEach((e) => e.remove()));
}
/** Pins the app's low-refresh motion policy (`window.__helmPerf.motion`), or hands it back to auto. */
async function forceMotion(c: "normal" | "low" | "auto"): Promise<void> {
  await page().evaluate((m) => (window as unknown as { __helmPerf?: { motion: { force(c: string): void } } }).__helmPerf?.motion.force(m), c);
}
async function runningAnimations(): Promise<number> {
  return page().evaluate(() => document.getAnimations().filter((a) => a.playState === "running" || a.playState === "pending").length);
}
async function animationNames(): Promise<string[]> {
  return page().evaluate(() =>
    document
      .getAnimations()
      .map((a) => ((a as CSSAnimation).animationName ?? (a as CSSTransition).transitionProperty ?? "?") + "@" + String((a.effect as KeyframeEffect | null)?.target?.className ?? ""))
      .slice(0, 8),
  );
}

async function audit(surface: string, opts: { focus?: boolean; textOnly?: boolean; chrome?: boolean } = {}): Promise<void> {
  const skip = opts.chrome ? [] : ["sidebar", "topbar"];
  for (const theme of THEMES) {
    await setLook(theme, "normal");
    await page().evaluate(MARK_SWATCHES);
    for (const media of MEDIA) {
      await setLook(theme, media);
      if (media === "forced") {
        for (const f of await page().evaluate(AUDIT_FORCED)) add({ surface, theme, media, kind: "forced", what: f });
      }
      const text = await page().evaluate(AUDIT_TEXT, [skip, null]);
      measured[`${surface}/${theme}/${media}/text`] = text.length;
      for (const t of text) {
        const need = t.size >= 24 || (t.size >= 18.66 && false) ? 3 : 4.5;
        // Under forced colours the system picks the colours; the check there is the focus and
        // shape one below, so contrast is judged in normal and increased contrast only.
        if (media !== "forced" && t.ratio < need) {
          add({ surface, theme, media, kind: "contrast", what: `${t.sel} "${t.text}" ${t.ratio}:1 < ${need} (${t.fg} on ${t.bg}, ${t.size}px${t.uncertain ? ", over an image" : ""})` });
        }
      }
      if (opts.focus && !opts.textOnly) {
        const stops = await focusStops();
        measured[`${surface}/${theme}/${media}/stops`] = stops.length;
        for (const s of stops) {
          if (!s.visible || skip.includes(s.region)) continue;
          if (s.clipped) add({ surface, theme, media, kind: "focus", what: `${s.name}: ring is cut off by ${s.clipBy} (${s.outline})` });
          const ring = s.outline !== "none" || s.boxShadow !== "none";
          if (s.outline !== "none" && media !== "forced" && s.ringRatio < 3) add({ surface, theme, media, kind: "focus", what: `${s.name}: ring ${s.ringRatio}:1 < 3 (${s.outline})` });
          if (!ring) add({ surface, theme, media, kind: "focus", what: `${s.name}: no outline or shadow when focused (border ${s.border}, bg ${s.bg})` });
        }
      }
    }
  }
  await setLook("light", "normal");
}

async function focusStops(max = 160): Promise<NonNullable<ReturnType<typeof AUDIT_FOCUS_STEP>>[]> {
  await page().evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  const stops: NonNullable<ReturnType<typeof AUDIT_FOCUS_STEP>>[] = [];
  const names = new Set<string>();
  for (let i = 0; i < max; i++) {
    await page().keyboard.press("Tab");
    const s = await page().evaluate(AUDIT_FOCUS_STEP);
    if (!s) break;
    const key = s.name + JSON.stringify(s.outline) + stops.length;
    if (names.has(s.name) && stops.length > 3 && stops.some((x) => x.name === s.name) && i > 8 && stops[0]!.name === s.name) break;
    names.add(key);
    stops.push(s);
  }
  return stops;
}

// ---- the sweep ------------------------------------------------------------------------------

describe("visual sweep", { timeout: 20 * 60_000 }, () => {
  const repo = scratchDir("helm-w5v-repo-");

  before(async () => {
    process.stderr.write(`w5-visual: profile ${profile}\n`);
    // A git repo with a staged edit, an unstaged edit and a new file, for the Changes view.
    const git = (...a: string[]) => execFileSync("git", ["-C", repo, "-c", "user.email=t@example.test", "-c", "user.name=t", ...a], { stdio: "ignore" });
    git("init", "-q");
    fs.writeFileSync(path.join(repo, "a.txt"), "one\ntwo\nthree\n");
    fs.writeFileSync(path.join(repo, "b.txt"), "alpha\nbeta\n");
    git("add", ".");
    git("commit", "-qm", "first");
    fs.writeFileSync(path.join(repo, "a.txt"), "one\n2\nthree\nfour\n");
    git("add", "a.txt");
    fs.writeFileSync(path.join(repo, "b.txt"), "alpha\nBETA\n");
    fs.writeFileSync(path.join(repo, "c.txt"), "new\n");
    app = await launchApp(profile);
    await page().setViewportSize({ width: 1280, height: 800 }).catch(() => undefined);
  });

  after(async () => {
    if (REPORT) fs.writeFileSync(REPORT, JSON.stringify({ findings, measured }, null, 1));
    await dispose(app);
  });

  test("empty window", async () => {
    await audit("empty", { focus: true, chrome: true });
  });

  test("a chat: transcript, tools, approval card, composer, queue", async () => {
    await newChat(page(), repo);
    await send(page(), "[E4] look around");
    await waitStatus(page(), "idle", 30_000);
    await send(page(), "[MD] show markdown");
    await waitStatus(page(), "idle", 30_000);
    await audit("transcript", { focus: true, chrome: true });
    await send(page(), "[E2] tidy the folder");
    await page().waitForSelector(tid("interaction-card"), { timeout: 30_000 });
    await audit("allow-card", { focus: true });
  });

  test("Reduce Motion: no animation or transition runs while a turn works", async () => {
    await newChat(page(), scratchDir("helm-w5v-chat-"));
    await send(page(), "[E8-run] keep working");
    await page().waitForSelector(tid("stop"), { timeout: 20_000 });
    // Low-refresh motion turns motion off by itself when frames come slowly, as they can in
    // an automated window; pin it to normal so only Reduce Motion decides here.
    const motionWas = await page().evaluate(() => document.documentElement.dataset.motion ?? "normal");
    await forceMotion("normal");
    await setLook("light", "normal");
    const live = await runningAnimations();
    await setLook("light", "normal", true);
    const reduced = await runningAnimations();
    process.stderr.write(`w5-visual: running animations while working: ${live} normally, ${reduced} under Reduce Motion (low-refresh motion was ${motionWas}; pinned to normal)\n`);
    assert.ok(live > 0, "the check is not vacuous: something animates while a turn works");
    assert.equal(reduced, 0, `Reduce Motion leaves no running animation: ${JSON.stringify(await animationNames())}`);
    // A toast eases in over 100 ms normally; under Reduce Motion it appears at once.
    await injectToasts();
    assert.equal(await runningAnimations(), 0, "a toast does not ease in under Reduce Motion");
    await dropToasts();
    await setLook("light", "normal");
    await injectToasts();
    assert.ok((await runningAnimations()) > 0, "normally a toast eases in (so the check above could fail)");
    await dropToasts();
    await forceMotion("auto");
  });

  test("queued messages, menus and pickers", async () => {
    // The chat from the Reduce Motion test is still working: what is sent now queues behind it.
    await send(page(), "queued one");
    await send(page(), "queued two");
    await page().waitForSelector(tid("queue-item"), { timeout: 20_000 });
    await audit("queue", { focus: true });
    // A chat row's menu, then the model and effort picker and the permission-mode picker.
    await page().click(`${tid("chat-item")}`, { button: "right" });
    await page().waitForSelector('[role="menu"]', { timeout: 5_000 });
    await audit("chat-menu", { textOnly: true });
    await page().keyboard.press("Escape");
    for (const picker of ["model-picker", "permission-picker"]) {
      await page().click(tid(picker));
      await page().waitForTimeout(300);
      await audit(picker, { textOnly: true });
      await page().keyboard.press("Escape");
    }
    // Go to file (Ctrl+P).
    await page().keyboard.press("Control+p");
    await page().waitForSelector(tid("quick-open"), { timeout: 5_000 });
    await audit("quick-open", { textOnly: true });
    await page().keyboard.press("Escape");
  });

  test("an error row and an interrupted turn", async () => {
    await newChat(page(), scratchDir("helm-w5v-chat-"));
    await send(page(), "[ERR] break");
    await waitStatus(page(), ["idle", "error", "failed"], 30_000).catch(() => undefined);
    await page().waitForTimeout(500);
    await audit("error-row", { focus: true });
  });

  test("toasts: text 4.5:1, icons 3:1, in light and dark", async () => {
    for (const theme of THEMES) {
      await setLook(theme, "normal");
      await injectToasts();
      const m = await page().evaluate(MEASURE_TOASTS, ".w5-toasts .ui-toast");
      measured[`toasts/${theme}`] = m;
      assert.ok(m.length >= 20, `toast parts were measured (${m.length})`);
      for (const part of m) {
        const need = part.part === "icon" ? 3 : 4.5;
        if (part.ratio < need) add({ surface: "toast", theme, media: "normal", kind: "contrast", what: `${part.kind} toast ${part.part} "${part.text}" ${part.ratio}:1 < ${need}` });
      }
      await dropToasts();
    }
    // The real bottom-right completion notice, from a chat that finished in the background.
    const bg = await newChat(page(), scratchDir("helm-w5v-chat-"));
    await send(page(), "[E1] say hello");
    await newChat(page(), scratchDir("helm-w5v-chat-"));
    await page().waitForSelector(tid("toast", `[data-chat-id="${bg}"]`), { timeout: 30_000 });
    for (const theme of THEMES) {
      await setLook(theme, "normal");
      const m = await page().evaluate(MEASURE_TOASTS, '[data-testid="toast"]');
      assert.ok(m.length >= 3, `completion notice parts were measured (${m.length})`);
      for (const part of m) {
        const need = part.part === "icon" ? 3 : 4.5;
        if (part.ratio < need) add({ surface: "completion-toast", theme, media: "normal", kind: "contrast", what: `${part.part} "${part.text}" ${part.ratio}:1 < ${need}` });
      }
    }
    await setLook("light", "normal");
  });

  test("settings panes", async () => {
    await page().keyboard.press("Escape");
    await page().keyboard.press("Control+Comma");
    await page().waitForSelector(tid("settings"), { timeout: 10_000 });
    const tabs = await page().$$eval(".settings-tab", (els) => els.map((e) => e.getAttribute("data-tab") ?? ""));
    for (const tab of tabs) {
      await page().click(`.settings-tab[data-tab="${tab}"]`);
      await page().waitForTimeout(250);
      await audit(`settings-${tab}`, { focus: true });
    }
    await page().keyboard.press("Escape");
  });

  test("search, side panel, terminal dock", async () => {
    await page().keyboard.press("Control+K");
    await page().waitForTimeout(300);
    await audit("search", { focus: true });
    await page().keyboard.press("Escape");
    await page().click(tid("panel-toggle"));
    await page().waitForSelector(tid("side-panel"), { timeout: 10_000 });
    await page().waitForTimeout(500);
    await audit("side-panel", { focus: true });
    await page().keyboard.press("Control+Backquote");
    await page().waitForSelector(tid("terminal-dock", '[data-open="true"]'), { timeout: 10_000 });
    await page().waitForTimeout(1500);
    await audit("terminal-dock", { focus: true });
    await page().keyboard.press("Control+Backquote");
  });

  test("390x844: nothing spills sideways and the chat stays readable", async () => {
    await page().setViewportSize({ width: 390, height: 844 }).catch(() => undefined);
    await page().waitForTimeout(500);
    const spill = await page().evaluate(() => {
      const w = innerWidth;
      const over = [...document.querySelectorAll<HTMLElement>("body *")]
        .filter((e) => {
          const r = e.getBoundingClientRect();
          const cs = getComputedStyle(e);
          return r.width > 0 && cs.visibility === "visible" && cs.position !== "fixed" && r.right > w + 1 && !e.closest("[hidden], .visually-hidden, .ui-toast-slot, [style*='display: none']");
        })
        .slice(0, 6)
        .map((e) => `${e.tagName.toLowerCase()}${e.className ? "." + String(e.className).split(" ")[0] : ""} right=${Math.round(e.getBoundingClientRect().right)}`);
      return { scrollWidth: document.documentElement.scrollWidth, w, over };
    });
    measured["narrow"] = spill;
    assert.ok(spill.scrollWidth <= spill.w + 1, `the page is ${spill.scrollWidth} px wide in a ${spill.w} px window: ${spill.over.join(", ")}`);
    await audit("narrow", { chrome: true });
    await page().setViewportSize({ width: 1280, height: 800 }).catch(() => undefined);
  });

  test("report", async () => {
    await shot(page(), OUT, "w5-visual-end");
    const known = (f: Finding) => KNOWN.find((k) => k.kind === f.kind && k.what.test(f.what));
    for (const f of findings) {
      const k = known(f);
      process.stderr.write(`${k ? `KNOWN (${k.item})` : "FINDING"} ${f.surface} ${f.theme}/${f.media} ${f.kind}: ${f.what}\n`);
    }
    const open = findings.filter((f) => !known(f));
    assert.equal(open.length, 0, `${open.length} findings`);
  });
});

export type { Page };
