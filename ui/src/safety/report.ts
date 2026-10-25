// Reports the page's own errors to the host's renderer error log (`ui_error_report`).
// The host rate limits and rotates the log; this side also stops sending after a cap per page load
// and skips the same message sent twice in a row within a few seconds, so a render loop costs
// almost nothing. Reporting never throws and never reports its own failures.

import type { Api } from '../api/client.ts';

export type ErrorKind = 'error' | 'rejection' | 'pane';

export const MAX_PER_PAGE = 50;
export const REPEAT_MS = 5_000;

/** The message and stack of anything that was thrown. */
export function describe(e: unknown): { message: string; stack: string } {
  if (e instanceof Error) return { message: `${e.name}: ${e.message}`, stack: e.stack ?? '' };
  if (typeof e === 'string') return { message: e, stack: '' };
  try {
    return { message: JSON.stringify(e) ?? String(e), stack: '' };
  } catch {
    return { message: String(e), stack: '' };
  }
}

export class ErrorReporter {
  private sent = 0;
  private last: { key: string; at: number } | null = null;

  private readonly send: (args: Record<string, string>) => Promise<unknown>;
  private readonly now: () => number;

  constructor(send: (args: Record<string, string>) => Promise<unknown>, now: () => number = Date.now) {
    this.send = send;
    this.now = now;
  }

  /** True when the report was sent on. */
  report(kind: ErrorKind, error: unknown, source = ''): boolean {
    const { message, stack } = describe(error);
    const key = `${kind} ${message}`;
    const at = this.now();
    if (this.sent >= MAX_PER_PAGE || (this.last && this.last.key === key && at - this.last.at < REPEAT_MS)) return false;
    this.sent += 1;
    this.last = { key, at };
    try {
      this.send({ kind, message, stack, source }).catch(() => undefined);
    } catch {
      /* a host that cannot be reached is not worth another error */
    }
    return true;
  }
}

let reporter: ErrorReporter | null = null;

/** Report an error a pane caught (or any other); a no-op before `installErrorReporting`. */
export function reportError(kind: ErrorKind, error: unknown, source = ''): void {
  reporter?.report(kind, error, source);
}

/** Listen for uncaught errors and unhandled rejections on `win` and send them to the host. */
export function installErrorReporting(api: Api, win: Pick<Window, 'addEventListener' | 'removeEventListener'> = window): () => void {
  reporter = new ErrorReporter((args) => api('ui_error_report', args));
  const onError = (ev: Event) => {
    const e = ev as ErrorEvent;
    reportError('error', e.error ?? e.message, e.filename ? `${e.filename}:${e.lineno}:${e.colno}` : '');
  };
  const onRejection = (ev: Event) => reportError('rejection', (ev as PromiseRejectionEvent).reason);
  win.addEventListener('error', onError);
  win.addEventListener('unhandledrejection', onRejection);
  return () => {
    win.removeEventListener('error', onError);
    win.removeEventListener('unhandledrejection', onRejection);
    reporter = null;
  };
}
