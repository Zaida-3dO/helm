// Helm's own browser: what the worker does when the broker's answer says a person is needed.
//
// The broker runs the user's own browser. Some steps stay the person's — a sign-in, or a
// consequential step on their attached browser, which comes back as `attached.needs_owner`. When
// that happens the chat's worker:
// - files the chat's attention item (the broker's own sentence, and the site's host when it saw
//   the navigation) so the chat lists under "needs you" on the PC and over the device link, and
// - asks on Telegram with "Done" / "Not yet", so the person can answer from wherever they are.
//   The tap comes back to the chat as a note; the agent then calls `browser_sign_in` with
//   action "done" for the same lease and carries on.
// - keeps the broker lease alive in the meantime: `browser_status` with the lease key is the
//   renew verb, and a person away from the PC should not lose their tab to a lapse. The hold
//   ends when that lease is marked done or released, when the cap passes, or when the worker
//   exits; the cap sits above the broker's longest sign-in wait, so a real wait is always held.
//
// The rules engage only for servers created from Helm's browser preset: a configured server
// whose tools merely share the broker's names never drives Helm's phone path.

import { readFileSync } from "node:fs";
import type { Json } from "./types.ts";

/** The preset id of the built-in browser server (the host's presets.rs keeps the same string). */
export const BROKER_PRESET = "browser_broker";

/** The broker's lease-key shape: the broker mints 32 random bytes in base64url (43 characters);
 * the range leaves a little room for a broker that changes its minting. */
const LEASE_KEY = /^[A-Za-z0-9_-]{32,128}$/;

/**
 * The lease keys an accepted broker answer hands out (`value.key`): from the structured content
 * and from every text block that parses as one. A caller may keep these from shape redaction —
 * they are handles the server hands out on purpose, not credentials. Never throws; no repeats.
 */
export function leaseKeysOf(raw: Json): string[] {
  const found: string[] = [];
  const note = (j: unknown): void => {
    if (!j || typeof j !== "object") return;
    const v = (j as Record<string, unknown>).value;
    if ((j as Record<string, unknown>).outcome !== "accepted" || !v || typeof v !== "object") return;
    const k = (v as Record<string, unknown>).key;
    if (typeof k === "string" && LEASE_KEY.test(k) && !found.includes(k)) found.push(k);
  };
  note(raw.structuredContent);
  const blocks = Array.isArray(raw.content) ? raw.content : [];
  for (const b of blocks) {
    if (!b || typeof b !== "object" || b.type !== "text" || typeof b.text !== "string") continue;
    try {
      note(JSON.parse(b.text));
    } catch {
      /* not JSON: nothing to keep */
    }
  }
  return found;
}

/** The broker caps a sign-in wait (default 600 s, maximum 900 s); hold a little past that. */
export const HOLD_CAP_MS = 15 * 60_000;
/** Lease lives 600 s without a call by default; renewing twice inside that is enough. */
export const RENEW_EVERY_MS = 45_000;

export interface HoldEvent {
  kind: "needs_owner" | "sign_in";
  serverId: string;
  leaseKey: string;
  /** The broker's own words for the person, already redacted by the bridge. */
  sentence: string;
}

function sentenceOf(text: string): string {
  // A successful sign-in request answers with an object carrying the sentence to relay; a
  // refusal is plain text. Either way: one trimmed line, never more than the phone fits.
  let s = text.trim();
  try {
    const j = JSON.parse(s) as Record<string, unknown>;
    for (const k of ["relay", "sentence", "message"]) {
      if (typeof j[k] === "string" && (j[k] as string).trim()) {
        s = (j[k] as string).trim();
        break;
      }
    }
  } catch {
    /* plain text */
  }
  return s.replace(/\s+/g, " ").slice(0, 240);
}

/**
 * The call that means a person is needed, or null. `lease_key` is required: an owner ask always
 * belongs to a lease, and one without it cannot be held or finished.
 */
export function holdEvent(
  serverId: string,
  tool: string,
  args: Record<string, unknown>,
  isError: boolean | undefined,
  text: string,
): HoldEvent | null {
  const lease = typeof args.lease_key === "string" ? args.lease_key : "";
  if (!lease) return null;
  if (tool === "browser_act" && isError && text.includes("needs_owner")) {
    return { kind: "needs_owner", serverId, leaseKey: lease, sentence: sentenceOf(text) };
  }
  if (tool === "browser_sign_in" && args.action === "request" && !isError) {
    return { kind: "sign_in", serverId, leaseKey: lease, sentence: sentenceOf(text) };
  }
  return null;
}

/** The calls that end a lease's hold: the person answered, or the lease is gone either way. */
export function holdFinished(tool: string, args: Record<string, unknown>, isError: boolean | undefined): boolean {
  if (isError) return false;
  if (tool === "browser_release") return true;
  return tool === "browser_sign_in" && args.action === "done";
}

// ── Approving one step from a card ─────────────────────────────────────────────────────────────
//
// A needs_owner refusal of a step on one named control (press, tick, choose, type) can be
// approved by the person from wherever they are. The worker reads what the control is and asks
// the host to show an Approve card; on Approve the host sends back a one-time grant on this
// worker's stdin, and the worker retries that exact step with it. The grant never reaches the
// agent: an `owner_grant` an agent sends is removed before the call goes out, and the answer to
// the retried step is handed to the host, which tells the agent as the card's outcome. Steps that
// are always the person's own (checkout, order, pay, transfer, closing an account) and sign-ins or
// secret fields are refused before any card exists; the host checks the same lists again.

interface WordClass {
  words: string[];
  pairs: [string, string][];
}

const WORDS = JSON.parse(readFileSync(new URL("./owner-step-words.json", import.meta.url), "utf8")) as { never: WordClass; secret: WordClass };

/** The broker verbs that act on one named control: the only ones a grant can answer for. */
export const STEP_VERBS = ["click", "check", "select", "type", "fill", "press"];

// Zero-width joins and spaces, the bidi controls and the soft hyphen: invisible to a reader.
const INVISIBLE = new RegExp(`[${[[0xad], [0x200b, 0x200f], [0x202a, 0x202e], [0x2060], [0xfeff]].map((r) => r.map((c) => String.fromCharCode(c)).join("-")).join("")}]`, "g");

/** Lower-case word tokens, folded the way the broker folds a control's name. */
export function tokens(s: string | undefined): string[] {
  if (!s) return [];
  const runs = s.replace(INVISIBLE, "").match(/[\p{L}\p{N}\p{M}]+/gu) ?? [];
  return runs.map((r) => r.normalize("NFKD").replace(/\p{Mn}/gu, "").toLowerCase()).filter((r) => r !== "");
}

function hits(c: WordClass, seen: Set<string>): boolean {
  return c.words.some((w) => seen.has(w)) || c.pairs.some(([a, b]) => seen.has(a) && seen.has(b));
}

export type StepClass = "never" | "secret" | "ask";

/** Where a step stands before any card: always the person's own, a sign-in or secret, or askable.
 * `text` is what the control shows when that differs from its accessible name, if the broker
 * reports it. */
export function stepClass(name: string, role: string, value: string | undefined, pageSecret: boolean, text?: string): StepClass {
  const seen = new Set([...tokens(name), ...tokens(role), ...tokens(text)]);
  if (hits(WORDS.never, seen)) return "never";
  if (hits(WORDS.secret, seen) || (value ?? "").includes("{{secret:") || pageSecret) return "secret";
  return "ask";
}

/** True when the page's own address says the step is the person's own — a checkout, a payment
 * or an order page. A false positive only refuses, which is the safe direction. */
export function urlNever(url: string | undefined): boolean {
  if (!url) return false;
  try {
    return hits(WORDS.never, new Set(tokens(new URL(url).pathname)));
  } catch {
    return false;
  }
}

/** A broker reference: letters, digits, `_` and `-`. */
export function plainRef(ref: unknown): ref is string {
  return typeof ref === "string" && /^[A-Za-z0-9_-]{1,32}$/.test(ref);
}

/** The control the call names, when it is one a card can approve: a verb on one reference. */
export function oneControl(args: Record<string, unknown>): { verb: string; ref: string; value?: string } | null {
  if (args.request !== undefined) return null;
  // The broker also reads these spellings, and prefers `ref` over `target`. A call that carries
  // one of them might act on a control the card never saw, so it gets no card.
  if (["ref", "key", "targetRef", "target_ref"].some((k) => args[k] !== undefined)) return null;
  const verb = typeof args.action === "string" ? args.action : "";
  if (!STEP_VERBS.includes(verb) || !plainRef(args.target)) return null;
  const value = typeof args.value === "string" ? args.value : undefined;
  return value === undefined ? { verb, ref: args.target } : { verb, ref: args.target, value };
}

/** The finder for one read that returns the control's own line and every text box on the page. */
export function controlFinder(ref: string): string {
  return `/\\[ref=${ref}\\]|textbox/`;
}

/** One snapshot line: `- role "name" [ref=e12] …`. The bracketed attributes after the name are
 * read as `key=value` pairs, quoted or bare; `text` is the text the control shows when it
 * differs from its accessible name, on brokers that report it. */
function lineParts(line: string): { role: string; name: string; text?: string } | null {
  const m = /^\s*-?\s*([A-Za-z][\w-]*)(?:\s+"((?:[^"\\]|\\.)*)")?/.exec(line);
  if (!m) return null;
  let name = m[2] ?? "";
  try {
    name = JSON.parse(`"${name}"`) as string;
  } catch {
    /* kept as written */
  }
  let text: string | undefined;
  for (const a of line.matchAll(/\[([A-Za-z][\w-]*)=([^\]]*)\]/g)) {
    if (a[1] === "text") {
      let v = a[2]!.trim();
      try {
        v = JSON.parse(v.startsWith('"') ? v : `"${v}"`) as string;
      } catch {
        /* kept as written */
      }
      text = v;
    }
  }
  return { role: m[1]!.toLowerCase(), name, ...(text !== undefined ? { text } : {}) };
}

/** What the snapshot says about `ref`, and whether the page shows a sign-in or secret field. */
export function readControl(snapshot: string, ref: string): { role: string; name: string; text?: string; pageSecret: boolean } | null {
  const lines = snapshot.split("\n");
  const own = lines.find((l) => l.includes(`[ref=${ref}]`));
  const parts = own ? lineParts(own) : null;
  if (!parts || !parts.name.trim()) return null;
  const pageSecret = lines.some((l) => {
    const p = lineParts(l);
    return !!p && p.role === "textbox" && hits(WORDS.secret, new Set(tokens(p.name)));
  });
  return { ...parts, pageSecret };
}

/** The broker's accepted answers are JSON text: `{outcome, value}`. */
export function acceptedValue(text: string): Record<string, unknown> | null {
  try {
    const j = JSON.parse(text) as Record<string, unknown>;
    return j.outcome === "accepted" && j.value && typeof j.value === "object" ? (j.value as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

/** The arguments with any owner grant removed: only Helm's own retry may carry one. */
export function withoutGrant(args: Record<string, unknown>): Record<string, unknown> {
  const { owner_grant: _a, ownerGrant: _b, ...rest } = args;
  return rest;
}
