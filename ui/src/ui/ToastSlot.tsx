// The main toast slot's view: fixed top-centre below the top bar.

import { useSyncExternalStore } from 'react';
import { Toast } from './misc.tsx';
import { toast } from './toasts.ts';

export function ToastSlotView() {
  const t = useSyncExternalStore(toast.subscribe, toast.current);
  if (!t) return null;
  return (
    <div className="ui-toast-slot" data-testid="toast-slot">
      <Toast
        key={t.id}
        kind={t.kind}
        testId="notice"
        icon={t.kind === 'error' ? 'alert' : 'info'}
        action={t.action ? { label: t.action.label, hint: t.action.hint, onClick: () => (toast.dismiss(t.id), t.action?.run()) } : undefined}
        onDismiss={() => toast.dismiss(t.id)}
      >
        {t.text}
      </Toast>
    </div>
  );
}
