// The worker's MCP bridge: holds the resolved server list from the host's
// `mcp_config` line, connects lazily per server, discovers and loads tools for the model, runs
// calls, and reports per-server status as `mcp_status` lines.
//
// - Limits and naming: at most 8 MCP requests run at once in a worker.
// - Redaction: every server's resolved secrets are scrubbed from names, descriptions, schemas,
//   results and errors before they leave the bridge.
// - A server configured both in Helm and in Claude Code (same normalised name or URL): Helm's
//   copy wins. Helm's client keeps serving it, and `shadowing` names it so the driver can refuse
//   calls to Claude Code's copy.
// - A config change replaces only the servers whose definition changed; their loaded tools are
//   dropped, so a child can call only tools that are discovered and still registered.

import fs from "node:fs";
import path from "node:path";
import {
  HOLD_CAP_MS,
  RENEW_EVERY_MS,
  acceptedValue,
  controlFinder,
  holdEvent,
  holdFinished,
  isBrokerServer,
  leaseKeysOf,
  oneControl,
  oneExpression,
  readControl,
  stepClass,
  urlNever,
  withoutGrant,
  type HoldEvent,
} from "./browser.ts";
import { HttpTransport, type Resolver } from "./http.ts";
import { generatedName, label, matches, terms, toolNameOk } from "./naming.ts";
import { formatResult } from "./output.ts";
import { Redactor } from "./redact.ts";
import { McpSession, type Transport } from "./session.ts";
import { StdioTransport } from "./stdio.ts";
import {
  LIMITS,
  McpError,
  type CallResult,
  type DiscoveryResult,
  type Json,
  type LoadedTool,
  type McpConfig,
  type McpLimits,
  type McpTool,
  type NativeServer,
  type ResolvedServer,
  type ServerState,
  type ServerStatus,
} from "./types.ts";

/** The host-side tool that fetches a current `Authorization` header for a browser-signed-in server.
 * Its answer says only that the header was sent: the header itself comes back on the worker's stdin
 * as an `mcp_oauth_header` line (see `deliverOauthHeader`), so it never sits in a file. */
const OAUTH_HEADER_OP = "app_mcp_oauth_header";
/** The host-side tools behind an Approve card for one browser step: file the card, and hand back
 * what the browser answered to the approved step. Neither is ever listed to the agent. */
const STEP_OP = "app_browser_step";
const STEP_RESULT_OP = "app_browser_step_result";

/** What a browser step that is always the person's own gets instead of a card. */
const NEVER_STEP =
  "This step is one the user always does themselves: Helm never approves a checkout, an order, a payment, a transfer or closing an account on their behalf, so no card was shown. Leave the browser where it is and tell the user it is ready for them.";

/** The text blocks of a result, joined. */
function textOf(r: { content: Json[] }): string {
  return r.content
    .filter((c) => c.type === "text")
    .map((c) => String(c.text ?? ""))
    .join(" ");
}

export interface BridgeOptions {
  /** The chat's folder: stdio servers start there. */
  cwd?: string;
  env?: NodeJS.ProcessEnv;
  limits?: Partial<McpLimits>;
  resolver?: Resolver;
  now?: () => number;
  /** Ask the host an op through the chat's request channel (the header ask above, and the
   * browser hold reports). */
  hostRequest?: (operation: string, args: Json, opts: { timeoutMs: number; mutating?: boolean }) => Promise<{ ok: boolean; result?: unknown; error?: string }>;
  /** Override for the browser lease hold (tests use short timings). */
  browser?: { renewMs?: number; capMs?: number };
}

export interface McpBridge {
  configure(cfg: McpConfig): void;
  /** The servers the CLI reported loading natively at start-up. */
  noteNative(servers: NativeServer[]): void;
  /** The Helm server that Claude Code's own server `nativeName` duplicates, if any. Helm's copy
   * takes precedence, so calls to Claude Code's copy are refused. */
  shadowing(nativeName: string): { id: string; name: string } | undefined;
  status(): ServerStatus[];
  /** The host's `mcp_oauth_header` line: hands the fresh `Authorization` value to whoever asked for
   * it for `serverId`. False when nothing was waiting (the value is dropped). */
  deliverOauthHeader(serverId: string, header: string): boolean;
  /** The host's `mcp_owner_grant` line: the person approved a browser step, and this is the
   * one-time grant to retry it with. False when no such step waits here. */
  deliverOwnerGrant(line: { requestId: string; serverId: string; leaseKey: string; ref: string; grant: string }): boolean;
  /** Ids `app_mcp_tools` may name: every enabled server. */
  serverIds(): string[];
  tools(args: { server: string; query?: string }): Promise<DiscoveryResult>;
  /** True when `name` is a discovered tool whose server is still configured unchanged. */
  isLoaded(name: string): boolean;
  call(name: string, args: Json, signal?: AbortSignal): Promise<CallResult>;
  close(): void;
}

/** Normalised server name: case, separators and punctuation folded. */
export function normName(n: string): string {
  return n.toLowerCase().replace(/[^a-z0-9]/g, "");
}

/** Normalised URL: lowercase origin plus path without a trailing slash; undefined if invalid. */
export function normUrl(u: string | undefined): string | undefined {
  if (!u) return undefined;
  try {
    const x = new URL(u);
    return `${x.protocol}//${x.host.toLowerCase()}${x.pathname.replace(/\/+$/, "")}`;
  } catch {
    return undefined;
  }
}

/** Native servers the CLI must not load: any named like Helm's own server. */
export function deniedNative(names: string[]): string[] {
  return names.filter((n) => normName(n) === "helm");
}

interface Entry {
  server: ResolvedServer;
  key: string;
  red: Redactor;
  session?: McpSession;
  connecting?: Promise<McpSession>;
  status: ServerStatus;
}

interface Loaded {
  serverId: string;
  key: string;
  tool: string;
  view: LoadedTool;
}

class Slots {
  private used = 0;
  private readonly queue: (() => void)[] = [];
  private readonly max: number;
  constructor(max: number) {
    this.max = max;
  }
  async run<T>(f: () => Promise<T>): Promise<T> {
    if (this.used >= this.max) await new Promise<void>((r) => this.queue.push(r));
    this.used++;
    try {
      return await f();
    } finally {
      this.used--;
      this.queue.shift()?.();
    }
  }
}

export function createMcpBridge(sink: (line: Json) => void, opts: BridgeOptions = {}): McpBridge {
  const limits: McpLimits = { ...LIMITS, ...(opts.limits ?? {}) };
  const now = opts.now ?? Date.now;
  const env = opts.env ?? process.env;
  const entries = new Map<string, Entry>();
  const loaded = new Map<string, Loaded>();
  const slots = new Slots(limits.maxConcurrent);
  let native: NativeServer[] = [];
  let closed = false;
  /** Header asks waiting for their `mcp_oauth_header` line, by server id. */
  const headerWaiters = new Map<string, ((header: string | undefined) => void)[]>();

  // ── The built-in browser: holding a lease while the person does their part ──────────
  // See `browser.ts`. A lease is renewed while its hold is open, and the chat's flag plus the
  // Telegram ask are raised through the host. Timings are overridable so the checks stay fast.
  const renewMs = opts.browser?.renewMs ?? RENEW_EVERY_MS;
  const capMs = opts.browser?.capMs ?? HOLD_CAP_MS;
  const holds = new Map<string, ReturnType<typeof setInterval>>();
  const holdStartedAt = new Map<string, number>();
  /** When this chat's browser asks went out in the last hour: a chat that keeps retrying a gated
   * action must not use up the shared Telegram budget. */
  const askTimes: number[] = [];
  const ASKS_PER_HOUR = 6;
  /** The host of the last navigation for a lease, to name the site in the report. */
  const leaseHosts = new Map<string, string>();
  const holdKey = (serverId: string, leaseKey: string) => `${serverId}\u0000${leaseKey}`;
  const stopHold = (key: string) => {
    const t = holds.get(key);
    if (t !== undefined) clearInterval(t);
    holds.delete(key);
    holdStartedAt.delete(key);
  };
  const stopHoldsFor = (serverId: string) => {
    for (const k of [...holds.keys()]) if (k.startsWith(`${serverId}\u0000`)) stopHold(k);
  };

  const startHold = (e: Entry, ev: HoldEvent) => {
    const key = holdKey(ev.serverId, ev.leaseKey);
    stopHold(key);
    holdStartedAt.set(key, now());
    const t = setInterval(() => {
      if (now() - (holdStartedAt.get(key) ?? 0) > capMs) {
        stopHold(key);
        return;
      }
      const s = e.session;
      if (!s || s.closed) return;
      // `browser_status` with the lease key is the broker's renew verb. A failed renewal is
      // left to the next tick: a dead lease stops needing one on its own.
      void s.callTool("browser_status", { lease_key: ev.leaseKey }).catch(() => {});
    }, renewMs);
    t.unref?.();
    holds.set(key, t);
  };

  /** The page each lease last arrived at (after redirects), the lease's purpose, and the steps
   * waiting on a card (by request id) or refused for good (by lease and reference). */
  const leaseUrls = new Map<string, string>();
  const leasePurposes = new Map<string, string>();
  interface Waiting {
    entry: Entry;
    tool: string;
    args: Json;
    leaseKey: string;
    /** The control's reference; empty for a step that runs an expression. */
    ref: string;
    /** What the agent's repeat of this step is matched on while the card waits. */
    guard: string;
    at: number;
  }
  const waitingSteps = new Map<string, Waiting>();
  const neverSteps = new Set<string>();
  const stepKey = (serverId: string, leaseKey: string, ref: string) => `${serverId}\u0000${leaseKey}\u0000${ref}`;
  /** An expression is matched on its exact text; the NUL keeps it apart from any plain reference. */
  const codeKey = (serverId: string, leaseKey: string, code: string) => stepKey(serverId, leaseKey, `evaluate\u0000${code}`);
  /** A refused-for-good entry belongs to the reference as it was numbered on the page then:
   * a navigation renumbers the references, and a released lease has no page at all. */
  const clearNever = (serverId: string, leaseKey: string) => {
    const p = `${serverId}\u0000${leaseKey}\u0000`;
    for (const k of [...neverSteps]) if (k.startsWith(p)) neverSteps.delete(k);
  };
  const waitingFor = (key: string) =>
    [...waitingSteps.values()].some((w) => w.guard === key && now() - w.at <= capMs);

  const reportHold = (ev: HoldEvent) => {
    if (!opts.hostRequest) return;
    const t = now();
    while (askTimes.length && t - askTimes[0]! > 3_600_000) askTimes.shift();
    if (askTimes.length >= ASKS_PER_HOUR) return;
    askTimes.push(t);
    const host = leaseHosts.get(holdKey(ev.serverId, ev.leaseKey));
    const where = host ? `${host}: ` : "";
    const question = ev.kind === "sign_in" ? "Your browser needs a sign-in" : "A browser step needs you";
    // The chat's attention item first: it lists the chat under "needs you" everywhere, the
    // phone included, whatever the Telegram ask below answers.
    void opts
      .hostRequest("app_flag_attention", { question, reason: `${where}${ev.sentence}`.slice(0, 240), priority: "high" }, { timeoutMs: 5_000, mutating: true })
      .catch(() => {});
    void opts
      .hostRequest("app_telegram_ask", { question: `${where}${ev.sentence}`.slice(0, 1000), options: ["Done", "Not yet"] }, { timeoutMs: 5_000, mutating: true })
      .catch(() => {});
  };

  const noteBrowserCall = (e: Entry, tool: string, args: Json, r: { content: Json[]; isError?: boolean }) => {
    const lease = typeof args.lease_key === "string" ? args.lease_key : "";
    if (!lease) return;
    const key = holdKey(e.server.id, lease);
    if (tool === "browser_navigate" && !r.isError && typeof args.url === "string") {
      // Where the tab arrived, after redirects, when the broker says; else where it was sent.
      const arrived = acceptedValue(textOf(r))?.url;
      const url = typeof arrived === "string" ? arrived : args.url;
      try {
        leaseHosts.set(key, new URL(url).host);
        leaseUrls.set(key, url);
      } catch {
        /* not a url worth naming */
      }
      clearNever(e.server.id, lease);
    }
    if ((tool === "browser_read" || tool === "browser_status") && !r.isError) {
      // The page's own answer keeps the lease's address current: a link click or a script
      // redirect moves the tab between navigations, and a card must name the site it is on.
      const u = acceptedValue(textOf(r))?.url;
      if (typeof u === "string") {
        try {
          leaseHosts.set(key, new URL(u).host);
          leaseUrls.set(key, u);
        } catch {
          /* not a url worth naming */
        }
      }
    }
    if (tool === "browser_release" && !r.isError) {
      leaseUrls.delete(key);
      leasePurposes.delete(key);
      clearNever(e.server.id, lease);
    }
    if (holdFinished(tool, args, r.isError)) {
      stopHold(key);
      leaseHosts.delete(key);
      return;
    }
    const ev = holdEvent(e.server.id, tool, args, r.isError, textOf(r));
    if (!ev) return;
    // One open ask per lease: a repeat refusal while the hold is open neither asks again nor
    // restarts the cap, which counts from the first event.
    if (holds.has(key)) return;
    startHold(e, ev);
    reportHold(ev);
  };

  /**
   * A needs_owner refusal of a step on one control: read what the control is and ask the host
   * for an Approve card. Answers the result to give the agent, or undefined to leave the call to
   * the owner-step hold above (no card could be shown, and the person does it at their browser).
   */
  const askForCard = async (
    e: Entry,
    tool: string,
    args: Json,
    r: { content: Json[]; isError?: boolean },
  ): Promise<{ content: Json[]; isError?: boolean } | undefined> => {
    const code = tool === "browser_evaluate" ? oneExpression(args) : null;
    const step = tool === "browser_evaluate" ? null : oneControl(args);
    const lease = typeof args.lease_key === "string" ? args.lease_key : "";
    const s = e.session;
    if ((!step && code === null) || !lease || !s || s.closed || !opts.hostRequest) return undefined;
    const key = holdKey(e.server.id, lease);
    const note = (why: string) => ({ content: [...r.content, { type: "text", text: `[Helm] ${why}` }], isError: true });
    if (code !== null) return askForCodeCard(e, tool, args, lease, code, note);
    if (!step) return undefined;
    // What the page says the control is, read the way the broker reads it.
    let control: ReturnType<typeof readControl> = null;
    let pageUrl: string | undefined;
    try {
      const read = formatResult(await s.callTool("browser_read", { lease_key: lease, find: controlFinder(step.ref), inline: true, limit: 16_000 }), e.red, limits);
      const val = acceptedValue(textOf(read));
      const u = val?.url;
      if (typeof u === "string" && u) pageUrl = u;
      const inlined = val?.inlined;
      const text = Array.isArray(inlined) ? inlined.map((x) => String((x as Json)?.text ?? "")).join("\n") : "";
      control = readControl(text, step.ref);
    } catch {
      /* unread: no card */
    }
    // The site as the broker's own page answer last said it, falling back to where the lease
    // last navigated: a card must not name a tab the page has already left.
    const where = pageUrl ?? leaseUrls.get(key);
    let host = "";
    try {
      host = new URL(where ?? "").hostname;
    } catch {
      /* the page's site is unknown: no card */
    }
    if (!control || !host) return undefined;
    const cls = stepClass(control.name, control.role, step.value, control.pageSecret, control.text);
    // The page's own address judges too: a button that only says "Confirm" on a checkout page
    // is still a checkout step.
    if (cls === "never" || urlNever(where)) {
      neverSteps.add(stepKey(e.server.id, lease, step.ref));
      return note(NEVER_STEP);
    }
    if (cls === "secret") return undefined;
    // A picture of the control, when the broker can take one and says where it put it.
    let picture: string | undefined;
    try {
      const cap = formatResult(await s.callTool("browser_capture", { lease_key: lease, selector: `aria-ref=${step.ref}` }), e.red, limits);
      const p = (acceptedValue(textOf(cap))?.capture as Json | undefined)?.path;
      const root = e.server.env?.BROKER_ARTIFACTS_ROOT;
      const abs = typeof p === "string" ? (path.isAbsolute(p) ? p : root ? path.join(root, p) : "") : "";
      if (abs && fs.existsSync(abs)) picture = abs;
    } catch {
      /* no picture: the card says so */
    }
    const asked: Json = { serverId: e.server.id, leaseKey: lease, ref: step.ref, verb: step.verb, name: control.name, role: control.role, host, pageSecret: control.pageSecret };
    if (step.value !== undefined) asked.value = step.value;
    const purpose = leasePurposes.get(key);
    if (purpose) asked.purpose = purpose;
    if (picture) asked.picture = picture;
    const filed = await opts.hostRequest(STEP_OP, asked, { timeoutMs: 30_000, mutating: true }).catch((err: Error) => ({ ok: false, error: err.message, result: undefined }));
    const result = filed.ok ? (filed.result as Json | undefined) : undefined;
    // The host refused (it checks the same lists, and caps the cards a chat may have waiting):
    // the person does the step at their browser, as without cards.
    if (typeof result?.requestId !== "string") return undefined;
    waitingSteps.set(result.requestId, { entry: e, tool, args, leaseKey: lease, ref: step.ref, guard: stepKey(e.server.id, lease, step.ref), at: now() });
    return cardFiled(e, lease, result, note);
  };

  /** The lease is kept alive while the card waits; the card itself is the person's ask. Answers
   * the result to give the agent. */
  const cardFiled = (e: Entry, lease: string, result: Json, note: (why: string) => { content: Json[]; isError?: boolean }) => {
    startHold(e, { kind: "needs_owner", serverId: e.server.id, leaseKey: lease, sentence: "" });
    const summary = typeof result.summary === "string" ? ` (${result.summary})` : "";
    return note(
      `Helm showed the user an Approve card for this step${summary}. End your turn now: when they approve, Helm makes this one step itself and the browser's answer reaches you as a note. Do not retry it, and do not call browser_sign_in for it.`,
    );
  };

  /**
   * A needs_owner refusal of one expression: ask the host for an Approve card that shows the
   * whole expression and the page's site. No control is read and no picture is taken; the grant
   * is for these exact characters, so the retry sends the call the agent made, unchanged.
   */
  const askForCodeCard = async (
    e: Entry,
    tool: string,
    args: Json,
    lease: string,
    code: string,
    note: (why: string) => { content: Json[]; isError?: boolean },
  ): Promise<{ content: Json[]; isError?: boolean } | undefined> => {
    const s = e.session;
    if (!s || !opts.hostRequest) return undefined;
    const key = holdKey(e.server.id, lease);
    // The site the lease last arrived at: the broker answers a navigation with the address the
    // tab reached, and no other answer names the page. A tab that has since moved on gives a
    // stale site, and the broker then refuses the grant rather than run the code.
    let host = "";
    try {
      host = new URL(leaseUrls.get(key) ?? "").hostname;
    } catch {
      /* the page's site is unknown: no card */
    }
    // No card can be made, and the owner has nothing to do at their browser: say so to the agent.
    if (!host) return note("Helm does not know which site this tab is on, so it could not show the user an Approve card for this code. Navigate the tab with browser_navigate first, then run it again. Do not call browser_sign_in for it.");
    const asked: Json = { serverId: e.server.id, leaseKey: lease, ref: "", verb: "evaluate", name: "", host, value: code };
    const purpose = leasePurposes.get(key);
    if (purpose) asked.purpose = purpose;
    const filed = await opts.hostRequest(STEP_OP, asked, { timeoutMs: 30_000, mutating: true }).catch((err: Error) => ({ ok: false, error: err.message, result: undefined }));
    const result = filed.ok ? (filed.result as Json | undefined) : undefined;
    // The host refused (code it cannot show whole, a stored secret, too many cards waiting). The
    // owner cannot run code at their browser, so no hold and no ask: the agent gets the reason.
    if (typeof result?.requestId !== "string") {
      const why = typeof filed.error === "string" && filed.error ? ` ${filed.error.slice(0, 600)}` : "";
      return note(`Helm could not show the user an Approve card for this code.${why} Do not call browser_sign_in for it.`);
    }
    waitingSteps.set(result.requestId, { entry: e, tool, args, leaseKey: lease, ref: "", guard: codeKey(e.server.id, lease, code), at: now() });
    return cardFiled(e, lease, result, note);
  };

  /** Start waiting for the host's header line for `serverId` (register before asking, so a line
   * that beats the reply to the ask is not missed). */
  const expectHeader = (serverId: string, ms: number): { done: Promise<string | undefined>; cancel: () => void } => {
    let resolve!: (h: string | undefined) => void;
    const done = new Promise<string | undefined>((r) => (resolve = r));
    const list = headerWaiters.get(serverId) ?? [];
    list.push(resolve);
    headerWaiters.set(serverId, list);
    const cancel = () => {
      clearTimeout(timer);
      const now = headerWaiters.get(serverId)?.filter((w) => w !== resolve) ?? [];
      if (now.length) headerWaiters.set(serverId, now);
      else headerWaiters.delete(serverId);
      resolve(undefined);
    };
    const timer = setTimeout(cancel, ms);
    return { done, cancel };
  };

  const emit = (e: Entry, state: ServerState, extra: { error?: string; toolCount?: number } = {}) => {
    const st: ServerStatus = { serverId: e.server.id, state, at: now() };
    if (extra.error !== undefined) st.error = e.red.text(extra.error).slice(0, 2000);
    if (extra.toolCount !== undefined) st.toolCount = extra.toolCount;
    e.status = st;
    sink({ type: "mcp_status", ...st });
  };

  /** The Helm entry that a Claude Code server named `nativeName` duplicates: the same normalised
   * name as the entry's id or name, or a URL reported for that name. A Helm server the host could
   * not resolve never shadows, so Claude Code's copy stays usable. */
  const shadowed = (nativeName: string): Entry | undefined => {
    const n = normName(nativeName);
    if (!n) return undefined;
    const urls = new Set(native.filter((x) => normName(x.name) === n).map((x) => normUrl(x.url)).filter(Boolean));
    for (const e of entries.values()) {
      if (e.server.error) continue;
      const u = normUrl(e.server.url);
      if (normName(e.server.id) === n || normName(e.server.name) === n || (u !== undefined && urls.has(u))) return e;
    }
    return undefined;
  };

  const dropLoaded = (serverId: string) => {
    for (const [k, v] of loaded) if (v.serverId === serverId) loaded.delete(k);
  };

  const retire = (e: Entry) => {
    stopHoldsFor(e.server.id);
    e.session?.close("configuration changed");
    e.session = undefined;
    e.connecting = undefined;
    dropLoaded(e.server.id);
  };

  const transportFor = (s: ResolvedServer): Transport & { start?: () => void } => {
    if (s.transport === "stdio") {
      const red = entries.get(s.id)?.red;
      return new StdioTransport(s, { cwd: opts.cwd, env, maxRecordBytes: limits.maxRecordBytes, redact: red ? (x) => red.text(x) : undefined });
    }
    const e = entries.get(s.id);
    const oauthHeader =
      s.oauth && opts.hostRequest && e
        ? async (refused?: string): Promise<string | undefined> => {
            const waiting = expectHeader(s.id, 30_000);
            let header: string | undefined;
            try {
              const r = await opts.hostRequest!(OAUTH_HEADER_OP, refused ? { serverId: s.id, refused } : { serverId: s.id }, { timeoutMs: 30_000 });
              if (r.ok) header = await waiting.done;
            } finally {
              waiting.cancel();
            }
            if (typeof header !== "string" || header === "") return undefined;
            // The fresh token joins this server's redaction before anything can echo it.
            e.red.add(header);
            const bearer = /^Bearer\s+(.+)$/i.exec(header);
            if (bearer?.[1]) e.red.add(bearer[1]);
            return header;
          }
        : undefined;
    return new HttpTransport(s, { limits, resolver: opts.resolver, oauthHeader });
  };

  const connect = (e: Entry): Promise<McpSession> => {
    if (e.session && !e.session.closed) return Promise.resolve(e.session);
    if (e.connecting) return e.connecting;
    if (e.server.error) return Promise.reject(new McpError(e.server.error));
    emit(e, "connecting");
    const attempt = (async () => {
      const t = transportFor(e.server);
      const session = new McpSession(t, limits, e.server.name);
      t.start?.();
      await session.initialize();
      return session;
    })();
    e.connecting = attempt;
    attempt.then(
      (session) => {
        if (e.connecting !== attempt) {
          session.close("configuration changed");
          return;
        }
        e.connecting = undefined;
        e.session = session;
        session.onEnd = (why) => {
          if (e.session !== session) return;
          e.session = undefined;
          if (why !== "closed" && why !== "configuration changed") emit(e, "failed", { error: why });
        };
        emit(e, "connected");
      },
      (err: unknown) => {
        if (e.connecting !== attempt) return;
        e.connecting = undefined;
        const signin = err instanceof McpError && err.signin && e.server.oauth;
        emit(e, signin ? "signin_required" : "failed", { error: (err as Error).message });
      },
    );
    return attempt;
  };

  const publicError = (e: Entry, err: unknown): McpError => {
    const m = err instanceof McpError ? err : new McpError(String((err as Error)?.message ?? err), { changed: "maybe" });
    return new McpError(e.red.text(m.message), { changed: m.changed, signin: m.signin });
  };

  /** A refused header on a browser-signed-in server is the user's to fix: say so once, and
   * drop the session so the next use reconnects and may ask the host again. */
  const noteSignin = (e: Entry, pe: McpError) => {
    if (pe.signin && e.server.oauth && e.status.state !== "signin_required") {
      e.session?.close();
      emit(e, "signin_required", { error: pe.message });
    }
  };

  const entryFor = (id: string): Entry => {
    const e = entries.get(id);
    if (!e) throw new McpError(`No enabled MCP server has the id ${id}.`);
    return e;
  };

  return {
    configure(cfg: McpConfig) {
      if (closed) return;
      const next = new Map<string, ResolvedServer>();
      // A host-served server (a Google accounts entry without a client of its own) is never a
      // process and never a failure: it gets no entry, no status row and no tools at all.
      for (const s of cfg.servers ?? [])
        if (s && typeof s.id === "string" && !s.hostServed) next.set(s.id, s);
      for (const [id, e] of entries) {
        if (!next.has(id)) {
          retire(e);
          entries.delete(id);
        }
      }
      if (cfg.native) native = cfg.native;
      for (const [id, s] of next) {
        const key = JSON.stringify(s);
        const old = entries.get(id);
        if (old && old.key === key) continue;
        if (old) retire(old);
        const e: Entry = {
          server: s,
          key,
          red: new Redactor(s.secrets ?? []),
          status: { serverId: id, state: "idle", at: now() },
        };
        entries.set(id, e);
        // A browser-signed-in server with no sign-in yet needs the user, not a failure report.
        if (s.error) emit(e, s.oauth ? "signin_required" : "failed", { error: s.error });
        else emit(e, "idle");
      }
    },

    noteNative(servers: NativeServer[]) {
      native = servers;
    },

    shadowing(nativeName: string) {
      const e = shadowed(nativeName);
      return e ? { id: e.server.id, name: e.server.name } : undefined;
    },

    status() {
      return [...entries.values()].map((e) => e.status);
    },

    deliverOauthHeader(serverId: string, header: string) {
      const list = headerWaiters.get(serverId);
      if (!list?.length) return false;
      headerWaiters.delete(serverId);
      for (const w of list) w(header);
      return true;
    },

    serverIds() {
      return [...entries.keys()];
    },

    async tools({ server, query }) {
      const e = entryFor(server);
      if (query !== undefined && query.length > 200) throw new McpError("The query is at most 200 characters.");
      return slots.run(async () => {
        let list: McpTool[];
        try {
          const session = await connect(e);
          list = await session.listTools();
        } catch (err) {
          const pe = publicError(e, err);
          noteSignin(e, pe);
          throw new McpError(pe.message);
        }
        emit(e, "connected", { toolCount: list.length });
        const t = terms(query, e.server.id);
        // A tool whose name reveals a secret cannot be renamed safely, so it is not offered.
        const safe = list.filter((tool) => toolNameOk(tool.name) && !e.red.reveals(tool.name));
        const hits = safe.filter((tool) => matches(t, [tool.name, tool.description, tool.title, label(e.server.name, tool.name)]));
        const out: LoadedTool[] = [];
        for (const tool of hits) {
          if (out.length >= limits.maxPerDiscovery) break;
          const name = generatedName(e.server.id, tool.name);
          const have = loaded.get(name);
          if (!have && loaded.size >= limits.maxLoaded) break;
          const view: LoadedTool = {
            name,
            label: e.red.text(label(e.server.name, tool.name)),
            description: e.red.text(
              `${tool.description ?? tool.title ?? ""}\n\nA cancelled or failed call does not prove a write did not happen.`.trim(),
            ),
            inputSchema: e.red.value(tool.inputSchema),
          };
          loaded.set(name, { serverId: e.server.id, key: e.key, tool: tool.name, view });
          out.push(view);
        }
        const result: DiscoveryResult = { server: e.server.id, total: safe.length, matches: hits.length, loaded: out, more: hits.length > out.length };
        if (e.server.guidance) result.guidance = e.server.guidance;
        return result;
      });
    },

    isLoaded(name: string) {
      const l = loaded.get(name);
      const e = l && entries.get(l.serverId);
      return !!(l && e && e.key === l.key);
    },

    async call(name: string, args: Json, signal?: AbortSignal) {
      const l = loaded.get(name);
      if (!l) throw new McpError(`${name} has not been discovered: load it with app_mcp_tools first.`);
      const e = entries.get(l.serverId);
      if (!e || e.key !== l.key) {
        loaded.delete(name);
        throw new McpError(`${name} is no longer available because its server changed. Discover it again with app_mcp_tools.`);
      }
      const browser = isBrokerServer(e.server);
      const source = { kind: "mcp" as const, server: e.red.text(e.server.name), tool: e.red.text(l.tool) };
      // Only Helm's own retry may carry an owner grant: whatever an agent sends is dropped.
      const sent = (browser ? withoutGrant(args ?? {}) : (args ?? {})) as Json;
      if (browser && l.tool === "browser_act") {
        // A step already refused for good, or already waiting on a card, never reaches the
        // browser again from the agent.
        const step = oneControl(sent);
        const k = step ? stepKey(e.server.id, typeof sent.lease_key === "string" ? sent.lease_key : "", step.ref) : "";
        if (k && neverSteps.has(k)) return { content: [{ type: "text", text: `[Helm] ${NEVER_STEP}` }], isError: true, source };
        if (k && waitingFor(k))
          return {
            content: [{ type: "text", text: "[Helm] An Approve card for this step is already waiting for the user. End your turn; their answer reaches you as a note." }],
            isError: true,
            source,
          };
      }
      if (browser && l.tool === "browser_evaluate") {
        // The same code, asked again while its card waits, never reaches the browser again.
        const code = oneExpression(sent);
        if (code !== null && waitingFor(codeKey(e.server.id, typeof sent.lease_key === "string" ? sent.lease_key : "", code)))
          return {
            content: [{ type: "text", text: "[Helm] An Approve card for this code is already waiting for the user. End your turn; their answer reaches you as a note." }],
            isError: true,
            source,
          };
      }
      return slots.run(async () => {
        let raw: Json;
        try {
          const session = await connect(e);
          raw = await session.callTool(l.tool, sent, signal);
        } catch (err) {
          const pe = publicError(e, err);
          noteSignin(e, pe);
          throw pe;
        }
        // The broker hands out its lease key on purpose: only that server's own accepted keys,
        // in the broker's minting shape, skip the shape rules (see `leaseKeysOf` and `Redactor.text`).
        let r: { content: Json[]; isError?: boolean } = formatResult(raw, e.red, limits, browser ? leaseKeysOf(raw) : []);
        if (browser) {
          if (l.tool === "browser_claim" && !r.isError && typeof sent.purpose === "string") {
            const leaseKey = acceptedValue(textOf(r))?.key;
            if (typeof leaseKey === "string") leasePurposes.set(holdKey(e.server.id, leaseKey), sent.purpose);
          }
          const ev = holdEvent(e.server.id, l.tool, sent, r.isError, textOf(r));
          const carded = ev?.kind === "needs_owner" ? await askForCard(e, l.tool, sent, r) : undefined;
          if (carded) r = carded;
          else noteBrowserCall(e, l.tool, sent, r);
        }
        return { content: r.content, isError: r.isError || undefined, source };
      });
    },

    deliverOwnerGrant(line) {
      const w = waitingSteps.get(line.requestId);
      if (!w || w.entry.server.id !== line.serverId || w.leaseKey !== line.leaseKey || w.ref !== line.ref) return false;
      // One grant, one retry: the step stops waiting whatever happens next.
      waitingSteps.delete(line.requestId);
      const e = w.entry;
      const report = (ok: boolean, text: string) =>
        void opts
          .hostRequest?.(STEP_RESULT_OP, { requestId: line.requestId, ok, text: text.split(line.grant).join("[grant]").slice(0, 8000) }, { timeoutMs: 10_000 })
          .catch(() => {});
      void (async () => {
        stopHold(holdKey(e.server.id, w.leaseKey));
        if (now() - w.at > capMs || entries.get(e.server.id) !== e) {
          report(false, "The browser server changed, or the step waited too long; nothing was retried.");
          return;
        }
        try {
          const r = await slots.run(async () => formatResult(await (await connect(e)).callTool(w.tool, { ...w.args, owner_grant: line.grant }), e.red, limits));
          report(!r.isError, textOf(r));
        } catch (err) {
          report(false, publicError(e, err).message);
        }
      })();
      return true;
    },

    close() {
      closed = true;
      for (const e of entries.values()) retire(e);
      for (const k of [...holds.keys()]) stopHold(k);
      waitingSteps.clear();
      entries.clear();
      loaded.clear();
    },
  };
}
