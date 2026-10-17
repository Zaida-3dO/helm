// The permission-mode chip: an indicator, not a picker. It shows the live
// mode the CLI reports; with none reported yet it shows bypass, since Skip permission prompts is on
// by default. There is no Settings pane yet, so it opens nothing.

import { permissionLabel } from './permission.ts';

export function PermissionChip({ mode }: { mode: string | null }) {
  const open = !mode || mode === 'bypassPermissions';
  return (
    <span
      className={`chip ${open ? 'chip-open' : 'chip-guarded'}`}
      data-testid="permission-chip"
      data-mode={mode ?? ''}
      title={mode ? `Live mode reported by Claude Code: ${permissionLabel(mode)}` : 'Applies when the task starts'}
    >
      {permissionLabel(mode)}
    </span>
  );
}
