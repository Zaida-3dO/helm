// The device toggle at the right of the Projects / Activity row: the computer whose chats the
// sidebar shows, with a dot for whether it is online. Drawn only when the hub has another computer.
// A click opens the list (this PC first, then the others A to Z); choosing one is remembered.

import { useRef, useState } from 'react';
import type { Api } from '../api/client.ts';
import type { EventBus } from '../api/events.ts';
import { ContextMenu, type MenuItem } from '../ui/index.ts';
import { IconMonitor } from '../ui/icons.tsx';
import type { DeviceInfo } from './api.ts';
import { useDevices } from './hooks.ts';
import { menuLabel, menuOrder, showToggle, stateText } from './model.ts';
import { errorText } from './store.ts';

function Dot({ online }: { online: boolean }) {
  return <span className="sb-device-dot" data-online={online ? 'true' : 'false'} aria-hidden="true" />;
}

export function DeviceToggle({ api, bus, onError }: { api: Api; bus: EventBus; onError?: (text: string) => void }) {
  const { store, snap } = useDevices(api, bus);
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = snap.list;
  if (!list || !showToggle(list)) return null;
  const current = list.devices.find((d) => d.uid === snap.selected) ?? list.devices.find((d) => d.uid === list.self);
  if (!current) return null;

  const now = Date.now();
  const choose = (d: DeviceInfo) => store.select(d.uid).catch((e: unknown) => onError?.(errorText(e)));
  const items: MenuItem[] = menuOrder(list).map((d) => ({
    id: `device-item-${d.uid}`,
    checked: d.uid === current.uid,
    label: (
      <span className="sb-device-item" data-testid={`device-item-${d.uid}`} data-online={d.online || d.isSelf ? 'true' : 'false'}>
        <Dot online={d.online || d.isSelf} />
        <span className="sb-device-item-text">
          <span className="sb-device-item-name">{menuLabel(d)}</span>
          {!d.isSelf && !d.online && <span className="sb-device-item-sub">{stateText(d, now)}</span>}
        </span>
      </span>
    ),
    onSelect: () => void choose(d),
  }));

  return (
    <>
      <button
        ref={button}
        type="button"
        className="sb-device"
        data-testid="device-toggle"
        aria-haspopup="menu"
        aria-expanded={menu !== null}
        aria-label={`Showing chats on ${current.name}`}
        title="Choose whose chats to show: this PC or another device"
        onClick={() => {
          const r = button.current?.getBoundingClientRect();
          setMenu({ x: r?.left ?? 0, y: r?.bottom ?? 0 });
        }}
      >
        <IconMonitor size={12} />
        <span className="sb-device-name">{current.name}</span>
        <Dot online={current.online || current.isSelf} />
      </button>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={items} label="Devices" onClose={() => setMenu(null)} testId="device-menu" />}
    </>
  );
}
