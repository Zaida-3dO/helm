// The top bar's "Restart to update" pill. It shows only when the update folder holds a newer
// setup file. A click asks the host what the restart would stop, then confirms in a dialog.
// Confirming saves drafts, closes Helm and runs the setup file (the host does all of it).

import { useCallback, useEffect, useState } from 'react';
import { updateOps, type UpdatePrepare, type UpdateStatus } from '../api/updates.ts';
import { useShell } from '../shell/context.ts';
import { ConfirmDialog, toast, type ConfirmSpec } from '../ui/index.ts';
import { busyLines, installProblem, restartBody } from './model.ts';
import './updates.css';

const err = (e: unknown) => toast.show({ kind: 'error', text: String((e as Error).message ?? e) });

/** The host's view of the update folder, kept fresh by the host's `update` event. */
export function useUpdateStatus(): UpdateStatus | null {
  const shell = useShell();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    const ops = updateOps(shell.api);
    const read = () => void ops.status().then(setStatus).catch(() => undefined);
    read();
    return shell.bus.on('update', read);
  }, [shell]);
  return status;
}

function RestartBody({ p }: { p: UpdatePrepare }) {
  return (
    <>
      {restartBody(p)}
      {p.busyTotal > 0 && (
        <span className="update-busy" data-testid="update-busy">
          {busyLines(p).map((l, i) => (
            <span key={i}>{l}</span>
          ))}
        </span>
      )}
    </>
  );
}

export function UpdatePill() {
  const shell = useShell();
  const status = useUpdateStatus();
  const [confirm, setConfirm] = useState<ConfirmSpec | null>(null);
  const available = status?.available ?? null;
  const open = useCallback(() => {
    const ops = updateOps(shell.api);
    ops
      .prepare()
      .then((p) =>
        setConfirm({
          title: 'Restart to update',
          body: <RestartBody p={p} />,
          confirm: 'Restart and update',
          onConfirm: () =>
            ops
              .install()
              .then(({ outcome }) => {
                const problem = installProblem(outcome);
                if (problem) toast.show({ kind: 'error', text: problem });
              })
              .catch(err),
        }),
      )
      .catch(err);
  }, [shell]);
  if (!available) return null;
  return (
    <>
      <button type="button" className="update-pill" data-testid="update-pill" title={`Helm ${available.version} is ready in your update folder`} onClick={open}>
        Restart to update
      </button>
      {confirm && <ConfirmDialog spec={confirm} onClose={() => setConfirm(null)} />}
    </>
  );
}
