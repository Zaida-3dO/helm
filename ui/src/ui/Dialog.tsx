// Dialogs: the centred dialog and the anchored (popover-form) dialog.
//
// - Focus: on open it goes to the first [autofocus] element, else the first field, else the first
//   button; Tab is trapped; on close focus returns to what had it before.
// - Escape comes through the layer stack (kind "dialog"); a busy dialog ignores Escape and the
//   backdrop.
// - A press directly on the backdrop dismisses.

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './controls.tsx';
import { useLayer } from './hooks.ts';
import { layoutSize, pageZoom, placeBelow, unzoom, viewportAt, type Rect } from './place.ts';

const FOCUSABLE =
  'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex="-1"])';

function useFocusTrap(panel: React.RefObject<HTMLElement | null>) {
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    const el = panel.current;
    const first =
      el?.querySelector<HTMLElement>('[autofocus], [data-autofocus]') ??
      el?.querySelector<HTMLElement>('input:not([disabled]), select, textarea') ??
      el?.querySelector<HTMLElement>(FOCUSABLE);
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Tab' || !el) return;
      const items = [...el.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((x) => x.offsetParent !== null);
      if (items.length === 0) return;
      const i = items.indexOf(document.activeElement as HTMLElement);
      const next = e.shiftKey ? (i <= 0 ? items.length - 1 : i - 1) : i === items.length - 1 ? 0 : i + 1;
      e.preventDefault();
      items[next]?.focus();
    };
    el?.addEventListener('keydown', onKey);
    return () => {
      el?.removeEventListener('keydown', onKey);
      if (before && document.contains(before)) before.focus();
    };
  }, [panel]);
}

export interface DialogProps {
  title: string;
  onClose: () => void;
  /** 620 px instead of 430 px. */
  wide?: boolean;
  /** While busy, Escape and the backdrop do nothing. */
  busy?: boolean;
  intro?: ReactNode;
  footer?: ReactNode;
  headerActions?: ReactNode;
  testId?: string;
  children?: ReactNode;
}

export function Dialog({ title, onClose, wide, busy, intro, footer, headerActions, testId, children }: DialogProps) {
  const panel = useRef<HTMLDivElement>(null);
  useFocusTrap(panel);
  useLayer('dialog', true, () => {
    if (busy) return true; // swallowed: a busy dialog ignores Escape
    onClose();
  });
  return createPortal(
    <div
      className="ui-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div ref={panel} className={`ui-dialog${wide ? ' wide' : ''}`} role="dialog" aria-modal="true" aria-label={title} aria-busy={busy || undefined} data-testid={testId}>
        <header className="ui-dialog-head">
          <h2>{title}</h2>
          <div className="ui-dialog-head-actions">
            {headerActions}
            <button type="button" className="ui-btn ui-btn-icon" aria-label="Close dialog" title="Close" onClick={onClose} disabled={busy}>
              <Icon name="x" size={17} />
            </button>
          </div>
        </header>
        {intro && <div className="ui-dialog-intro">{intro}</div>}
        <div className="ui-dialog-body">{children}</div>
        {footer && <footer className="ui-dialog-foot">{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}

export interface AnchoredDialogProps extends Omit<DialogProps, 'wide'> {
  /** The control it opens from (or the context menu it replaces). */
  anchor: Rect;
}

/** The anchored form: 344 px, no visible backdrop, placed by its trigger. */
export function AnchoredDialog({ title, onClose, busy, anchor, intro, footer, testId, children }: AnchoredDialogProps) {
  const panel = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  useFocusTrap(panel);
  useLayer('dialog', true, () => {
    if (busy) return true;
    onClose();
  });
  useLayoutEffect(() => {
    const el = panel.current;
    if (!el) return;
    const z = pageZoom();
    setPos(placeBelow(unzoom(anchor, z), layoutSize(el), viewportAt(z)));
  }, [anchor]);
  return createPortal(
    <div
      className="ui-backdrop clear"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        ref={panel}
        className="ui-dialog anchored"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        data-testid={testId}
        style={{ left: pos?.left ?? anchor.left, top: pos?.top ?? anchor.top + anchor.height + 6, visibility: pos ? 'visible' : 'hidden' }}
      >
        <header className="ui-dialog-head">
          <h2>{title}</h2>
        </header>
        {intro && <div className="ui-dialog-intro">{intro}</div>}
        <div className="ui-dialog-body">{children}</div>
        {footer && <footer className="ui-dialog-foot end">{footer}</footer>}
      </div>
    </div>,
    document.body,
  );
}
