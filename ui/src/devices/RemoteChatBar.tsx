// The strip at the top of a chat that runs on another device: where it runs and, when it cannot be
// changed from here, why. Mounted by ChatView for a chat whose host is not this PC.

import { IconMonitor } from '../ui/icons.tsx';
import type { RemoteChatState } from './state.ts';
import './remote.css';

export function RemoteChatBar({ state }: { state: RemoteChatState }) {
  return (
    <div className="remote-bar" data-testid="remote-chat-bar" data-state={state.state} data-tone={state.tone}>
      <span className="remote-bar-glyph" aria-hidden="true">
        <IconMonitor size={12} />
      </span>
      <span className="remote-bar-device">Runs on {state.deviceName}</span>
      {state.text && (
        <span className="remote-bar-state" data-testid="remote-chat-state" title={state.text}>
          {state.text}
        </span>
      )}
    </div>
  );
}
