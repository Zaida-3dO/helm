// The Settings → Hub line that says where this PC's pairing stands.

import type { HubConnection } from '../../api/hub.ts';

/** What the paired row says: the real state of the line, not just that a name was saved. `null`: nothing to say. */
export function pairedLine(conn: HubConnection | null): string | null {
  if (conn?.configProblem) return "The saved hub connection can't be read";
  if (!conn?.personName) return null;
  const who = `${conn.personName}${conn.machineName ? ` on ${conn.machineName}` : ''}`;
  switch (conn.state) {
    case 'online':
      return `Connected as ${who}`;
    case 'connecting':
      return `Paired as ${who}, connecting…`;
    case 'off':
      return `Paired as ${who}, but the hub line is not set up`;
    default:
      return `Paired as ${who}, but the hub is offline`;
  }
}

/** Whether the pairing exists but its line is dead: the row can offer to retry it now. */
export function canRetryLine(conn: HubConnection | null): boolean {
  return !!conn && conn.state === 'offline' && !!conn.personName;
}

/** What to say when a hub line stays offline: the hub restarts itself, so the person's own
 * step is only the last one, and only if that fails too. */
export const HUB_OFFLINE_STEPS =
  'If Tailscale is on and this keeps failing, the hub itself may be down. It restarts by itself within a few minutes; if it does not, restart the helm-hub container on the home server.';
