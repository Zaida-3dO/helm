// Redaction of resolved secrets. Every header and env value the host resolved for a
// server joins that server's set; anything a server sends back (tool names, descriptions,
// schemas, results, errors) passes through here before it reaches the model or the UI.

export const REDACTED = "[redacted]";

/** Secrets shorter than this are not redacted: replacing "a" everywhere would wreck output. */
const MIN_SECRET = 4;

/*
 * Shape-based redaction, for secrets Helm never resolved (a token a tool prints from its own
 * config, say). The rule is deliberately narrow, so ordinary output survives:
 *  - a long run (32 or more characters of base64, base64url or hex) is redacted only when it sits
 *    right after a keyword: token, key, secret, password, authorization, bearer, api_key or apikey
 *    (any case), separated by ":", "=" or a space, with optional quotes. A bare 40- or 64-character
 *    hex run is never touched on its own, so git commit ids and file hashes stay readable.
 *  - well-known prefixed tokens are redacted wherever they appear: sk-..., ghp_/gho_/ghu_/ghs_/ghr_
 *    and github_pat_..., xoxa-/xoxb-/xoxp-..., AKIA... and JWTs (eyJ... with two dots).
 */
const KEYWORD_RUN = /((?:token|key|secret|password|passwd|authorization|bearer|api_key|apikey)["']?(?:\s*[:=]\s*|\s+)["']?(?:bearer\s+)?)[A-Za-z0-9+/_=-]{32,}/gi;
const PREFIXED = new RegExp(
  String.raw`\b(?:` +
    [
    String.raw`sk-[A-Za-z0-9_-]{20,}`,
    String.raw`gh[pousr]_[A-Za-z0-9]{30,}`,
    String.raw`github_pat_[A-Za-z0-9_]{30,}`,
    String.raw`xox[abp]-[A-Za-z0-9-]{10,}`,
    String.raw`AKIA[0-9A-Z]{16}`,
    String.raw`eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*`,
    ].join("|") +
    ")",
  "g",
);

/** Redacts secret-looking runs by shape (see the rule above). */
export function redactShapes(text: string): string {
  return text.replace(KEYWORD_RUN, `$1${REDACTED}`).replace(PREFIXED, REDACTED);
}

export class Redactor {
  private secrets: string[];
  private readonly set = new Set<string>();

  constructor(secrets: Iterable<string>) {
    for (const s of secrets) this.collect(s);
    this.sort();
  }

  private collect(s: string): void {
    if (typeof s !== "string" || s.length < MIN_SECRET) return;
    this.set.add(s);
    // A secret may come back JSON- or URL-encoded inside text.
    const json = JSON.stringify(s).slice(1, -1);
    if (json !== s) this.set.add(json);
    const url = encodeURIComponent(s);
    if (url !== s) this.set.add(url);
  }

  private sort(): void {
    // Longest first, so a secret that contains another is replaced whole.
    this.secrets = [...this.set].sort((a, b) => b.length - a.length);
  }

  /** Adds a secret learned while running (a refreshed token), rebuilding the set. */
  add(secret: string): void {
    if (this.set.has(secret)) return;
    this.collect(secret);
    this.sort();
  }

  get empty(): boolean {
    return this.secrets.length === 0;
  }

  /** True when `text` contains any secret. */
  reveals(text: string): boolean {
    return this.secrets.some((s) => text.includes(s));
  }

  text(text: string): string {
    let out = text;
    for (const s of this.secrets) if (out.includes(s)) out = out.split(s).join(REDACTED);
    return redactShapes(out);
  }

  /**
   * Deep copy with every string redacted. Object keys that would reveal a secret are dropped
   * (a schema key is structure, so it cannot be rewritten into something meaningful).
   */
  value<T>(v: T): T {
    return this.walk(v, 0) as T;
  }

  private walk(v: unknown, depth: number): unknown {
    if (depth > 64) return null;
    if (typeof v === "string") return this.text(v);
    if (Array.isArray(v)) return v.map((x) => this.walk(x, depth + 1));
    if (v && typeof v === "object") {
      const out: Record<string, unknown> = {};
      for (const [k, x] of Object.entries(v as Record<string, unknown>)) {
        if (this.reveals(k)) continue;
        out[k] = this.walk(x, depth + 1);
      }
      return out;
    }
    return v;
  }
}
