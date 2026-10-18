// Settings → Usage → Agent feedback: a read-only view of what
// agents filed with app_feedback. Note text is other agents' words, so it renders as plain text
// only, never as Markdown or HTML. Export writes every entry as a JSON file.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { usageOps, type FeedbackFeature } from '../../api/usage.ts';
import { useShell } from '../../shell/context.ts';
import { Button, toast } from '../../ui/index.ts';

const SCORES = ['-2', '-1', '0', '1', '2'] as const;

export function FeedbackTab() {
  const shell = useShell();
  const ops = useMemo(() => usageOps(shell.api), [shell.api]);
  const [features, setFeatures] = useState<FeedbackFeature[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    ops.feedbackReport().then(
      (r) => {
        setFeatures(r.features);
        setError(null);
      },
      (e) => setError(String((e as Error)?.message ?? e)),
    );
  }, [ops]);

  useEffect(() => load(), [load]);

  const exportAll = () => {
    ops.feedbackExport().then(
      (r) => {
        const blob = new Blob([JSON.stringify(r.entries, null, 2)], { type: 'application/json' });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = 'helm-agent-feedback.json';
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 5_000);
        toast.show({ kind: 'info', text: `Exported ${r.entries.length} entr${r.entries.length === 1 ? 'y' : 'ies'}.` });
      },
      (e) => toast.show({ kind: 'error', text: String((e as Error)?.message ?? e) }),
    );
  };

  return (
    <section data-testid="agent-feedback">
      <div className="usage-pane-head">
        <h3>Agent feedback</h3>
        <span>
          <Button variant="text" onClick={load} data-testid="feedback-refresh">
            Refresh
          </Button>
          <Button variant="text" onClick={exportAll} disabled={!features || features.length === 0} data-testid="feedback-export">
            Export
          </Button>
        </span>
      </div>
      {error && (
        <p className="usage-notice" data-state="inactive" data-testid="feedback-error">
          {error}
        </p>
      )}
      {features && features.length === 0 && (
        <p className="usage-notice" data-state="inactive" data-testid="feedback-empty">
          No agent has rated a Helm feature yet. Agents file feedback with the app_feedback tool when a feature helps or gets in the way; it stays on this computer.
        </p>
      )}
      <div className="fb-list">
        {(features ?? []).map((f) => {
          const max = Math.max(1, ...SCORES.map((s) => f.histogram[s] ?? 0));
          return (
            <div className="fb-item" key={f.feature} data-testid="feedback-item">
              <div className="fb-head">
                <span className="fb-name">{f.feature}</span>
                <span className="fb-stats">
                  {f.count} rating{f.count === 1 ? '' : 's'} · mean {f.mean > 0 ? '+' : ''}
                  {f.mean}
                </span>
              </div>
              <div className="fb-hist" aria-label="Scores from -2 to 2">
                {SCORES.map((s) => (
                  <div className="fb-bin" key={s} title={`${f.histogram[s] ?? 0} at ${s}`}>
                    <span className="fb-bar" style={{ height: `${(100 * (f.histogram[s] ?? 0)) / max}%` }} />
                    <span>{s}</span>
                  </div>
                ))}
              </div>
              <div className="usage-fine">Kinds: {f.kinds.join(', ')}</div>
              {f.newestNotes.length > 0 && (
                <ul className="fb-notes">
                  {f.newestNotes.map((n, i) => (
                    <li key={i} data-testid="feedback-note">
                      {n}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}
