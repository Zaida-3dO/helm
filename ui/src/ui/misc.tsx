// Pill tabs, the custom tooltip and the toast box.

import { useRef, useState, type ReactNode } from 'react';
import { createPortal } from 'react-dom';
import { Icon } from './controls.tsx';
import { nextIndex, pageZoom } from './place.ts';

export interface Tab<T extends string> {
  id: T;
  label: ReactNode;
  disabled?: boolean;
}

/** Pill tabs (settings, inspector): arrow keys move between tabs. */
export function PillTabs<T extends string>({ tabs, value, onChange, label, testId }: { tabs: Tab<T>[]; value: T; onChange: (id: T) => void; label: string; testId?: string }) {
  const i = tabs.findIndex((t) => t.id === value);
  return (
    <div
      className="ui-tabs"
      role="tablist"
      aria-label={label}
      data-testid={testId}
      onKeyDown={(e) => {
        if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
        e.preventDefault();
        const n = nextIndex(i, e.key === 'ArrowRight' ? 1 : -1, tabs.map((t) => !!t.disabled));
        const t = tabs[n];
        if (t) onChange(t.id);
      }}
    >
      {tabs.map((t) => (
        <button
          key={t.id}
          type="button"
          role="tab"
          className="ui-tab"
          aria-selected={t.id === value}
          tabIndex={t.id === value ? 0 : -1}
          disabled={t.disabled}
          data-tab={t.id}
          onClick={() => onChange(t.id)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

/** A custom tooltip for the few places that need more than the native `title`. */
export function Tooltip({ text, children, delayMs = 400 }: { text: ReactNode; children: ReactNode; delayMs?: number }) {
  const host = useRef<HTMLSpanElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const show = () => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => {
      const r = host.current?.getBoundingClientRect();
      const z = pageZoom();
      if (r) setAt({ x: (r.left + r.width / 2) / z, y: r.top / z });
    }, delayMs);
  };
  const hide = () => {
    clearTimeout(timer.current);
    setAt(null);
  };
  return (
    <span ref={host} className="ui-tooltip-host" onMouseEnter={show} onMouseLeave={hide} onFocus={show} onBlur={hide}>
      {children}
      {at &&
        createPortal(
          <span className="ui-tooltip" role="tooltip" style={{ left: at.x, top: at.y }}>
            {text}
          </span>,
          document.body,
        )}
    </span>
  );
}

export interface ToastProps {
  icon?: 'alert' | 'info' | ReactNode;
  children: ReactNode;
  action?: { label: string; hint?: string; onClick: () => void; disabled?: boolean; title?: string };
  secondary?: { label: string; onClick: () => void };
  onDismiss?: () => void;
  testId?: string;
  kind?: string;
}

/** The toast box: always opaque, icon, one line of text, an optional text-button action. */
export function Toast({ icon = 'info', children, action, secondary, onDismiss, testId = 'toast', kind }: ToastProps) {
  return (
    <div className="ui-toast" role="status" aria-live="polite" data-testid={testId} data-kind={kind}>
      <span className="ui-toast-icon">{icon === 'alert' ? <Icon name="alert" size={16} /> : icon === 'info' ? <Icon name="info" size={16} /> : icon}</span>
      <span className="ui-toast-text">{children}</span>
      {action && (
        <button type="button" className="ui-btn ui-btn-text ui-toast-action" onClick={action.onClick} disabled={action.disabled} title={action.title}>
          {action.label}
          {action.hint && <kbd>{action.hint}</kbd>}
        </button>
      )}
      {secondary && (
        <button type="button" className="ui-btn ui-btn-text ui-toast-action" onClick={secondary.onClick}>
          {secondary.label}
        </button>
      )}
      {onDismiss && (
        <button type="button" className="ui-btn ui-btn-icon ui-toast-x" aria-label="Dismiss notice" title="Dismiss" onClick={onDismiss}>
          <Icon name="x" size={14} />
        </button>
      )}
    </div>
  );
}
