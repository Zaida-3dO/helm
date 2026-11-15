// The schedules view: the main area shows the schedules list in place of the chat, the way the
// new-chat page does. It closes on Escape, its close button, the tile, opening a chat or a new chat.

import { useEffect, useRef, useSyncExternalStore } from 'react';
import { newChat } from '../newchat/controller.ts';
import { SchedulesPane } from '../settings/schedules/SchedulesPane.tsx';
import { useSelection } from '../shell/context.ts';
import { Button, useLayer } from '../ui/index.ts';
import { IconX } from '../ui/icons.tsx';
import { schedulesView } from './view.ts';

export function SchedulesViewHost() {
  const open = useSyncExternalStore(schedulesView.subscribe, schedulesView.isOpen);
  const { chatId } = useSelection();
  const shownFor = useRef(chatId);
  // Opening a chat leaves the view; the view leaves the new-chat page (a typed draft waits for it).
  useEffect(() => {
    if (chatId !== shownFor.current) schedulesView.close();
    shownFor.current = chatId;
  }, [chatId]);
  useEffect(() => newChat.subscribe(() => newChat.open && schedulesView.close()), []);
  useEffect(() => {
    if (open) newChat.leave();
  }, [open]);
  useLayer('panel', open, () => schedulesView.close());
  if (!open) return null;
  // The close button hands focus back to the tile that opened the view, not to the page.
  const closeToTile = () => {
    schedulesView.close();
    document.querySelector<HTMLElement>('[data-testid=nav-schedules]')?.focus();
  };
  return (
    <section className="sched-view" aria-label="Schedules" data-testid="schedules-view">
      <header className="sched-view-head">
        <h2>Schedules</h2>
        <Button variant="icon" label="Close schedules" onClick={closeToTile} data-testid="schedules-view-close">
          <IconX size={17} />
        </Button>
      </header>
      <div className="sched-view-body">
        <SchedulesPane />
      </div>
    </section>
  );
}
