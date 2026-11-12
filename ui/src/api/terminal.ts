// The terminal's ops (helm-terminal).

import type { Api } from './client.ts';
import type { TerminalAttachResult, TerminalOpenArgs, TerminalSession } from './generated/terminal/index.ts';

export type { TerminalAttachResult, TerminalOpenArgs, TerminalSession };

export function terminalOps(api: Api) {
  return {
    open: (a: TerminalOpenArgs) => api<TerminalSession>('terminal_open', a),
    attach: (id: string) => api<TerminalAttachResult>('terminal_attach', { id }),
    write: (id: string, data: string) => api<null>('terminal_write', { id, data }),
    ack: (id: string, units: number) => api<null>('terminal_ack', { id, units }),
    resize: (id: string, cols: number, rows: number) => api<null>('terminal_resize', { id, cols, rows }),
    close: (id: string) => api<null>('terminal_close', { id }),
    sessions: () => api<TerminalSession[]>('terminal_sessions'),
    /** A new session running `claude auth login`; the host takes no program from here. */
    claudeSignIn: (cols = 100, rows = 24) => api<TerminalSession>('claude_sign_in', { cols, rows }),
  };
}

export type TerminalOps = ReturnType<typeof terminalOps>;
