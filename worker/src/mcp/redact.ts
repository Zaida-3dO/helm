// Redaction of resolved secrets. Every header and env value the host resolved for a
// server joins that server's set; anything a server sends back (tool names, descriptions,
// schemas, results, errors) passes through here before it reaches the model or the UI.

export const REDACTED = "[redacted]";

/** Secrets shorter than this are not redacted: replacing "a" everywhere would wreck output. */
const MIN_SECRET = 4;

export class Redactor {
  private readonly secrets: string[];

  constructor(secrets: Iterable<string>) {
    const set = new Set<string>();
    for (const s of secrets) {
      if (typeof s !== "string" || s.length < MIN_SECRET) continue;
      set.add(s);
      // A secret may come back JSON- or URL-encoded inside text.
      const json = JSON.stringify(s).slice(1, -1);
      if (json !== s) set.add(json);
      const url = encodeURIComponent(s);
      if (url !== s) set.add(url);
    }
    // Longest first, so a secret that contains another is replaced whole.
    this.secrets = [...set].sort((a, b) => b.length - a.length);
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
    return out;
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
