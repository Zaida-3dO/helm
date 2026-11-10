// The sidebar's resizer: a 1 px line with a 7 px hit area on the sidebar's right edge.
// Dragging writes the width every animation frame; pointer-up saves it; Escape, a window blur or a
// window resize during the drag puts the starting width back. Double-click does nothing.

import { useRef } from 'react';
import { dragWidth, sidebarLayout } from './layout.ts';

export function SidebarResizer() {
  const drag = useRef<{ start: number; x: number; main: number; frame: number; next: number; off: () => void } | null>(null);

  const end = (commit: boolean, el?: HTMLElement, pointerId?: number) => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    cancelAnimationFrame(d.frame);
    d.off();
    document.documentElement.classList.remove('is-col-resizing');
    if (el && pointerId !== undefined && el.hasPointerCapture(pointerId)) el.releasePointerCapture(pointerId);
    if (commit && d.next !== d.start) sidebarLayout.setWidth(d.next, true);
    else sidebarLayout.setWidth(d.start, false);
  };

  return (
    <div
      className="shell-resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize sidebar"
      data-testid="sidebar-resizer"
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        const el = e.currentTarget;
        const main = el.closest('.shell')?.querySelector<HTMLElement>('.shell-main')?.getBoundingClientRect().width ?? 0;
        const start = sidebarLayout.get().width;
        const cancel = () => end(false, el, e.pointerId);
        const onKey = (k: KeyboardEvent) => {
          if (k.key === 'Escape') {
            k.preventDefault();
            k.stopPropagation();
            cancel();
          }
        };
        window.addEventListener('keydown', onKey, true);
        window.addEventListener('blur', cancel);
        window.addEventListener('resize', cancel);
        drag.current = {
          start,
          x: e.clientX,
          main,
          frame: 0,
          next: start,
          off: () => {
            window.removeEventListener('keydown', onKey, true);
            window.removeEventListener('blur', cancel);
            window.removeEventListener('resize', cancel);
          },
        };
        el.setPointerCapture(e.pointerId);
        document.documentElement.classList.add('is-col-resizing');
        e.preventDefault();
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        d.next = dragWidth(d.start, e.clientX - d.x, d.main);
        cancelAnimationFrame(d.frame);
        d.frame = requestAnimationFrame(() => sidebarLayout.setWidth(d.next, false));
      }}
      onPointerUp={(e) => end(true, e.currentTarget, e.pointerId)}
      onPointerCancel={(e) => end(false, e.currentTarget, e.pointerId)}
    />
  );
}
