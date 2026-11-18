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

/** The preset id of the built-in browser server (the host's presets.rs keeps the same string). */
export const BROKER_PRESET = "browser_broker";

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
