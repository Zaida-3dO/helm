// The "(Review)" link after an "Answered for you" transcript row: it opens Settings →
// Gatekeeper, where that decision waits in the review list and the History.

import { SETTINGS_DIALOG } from '../settings/index.ts';
import { requestPane } from '../settings/registry.ts';
import { useShell } from '../shell/context.ts';

export const GATEKEEPER_PANE = 'gatekeeper';

export function GatekeeperReviewLink() {
  const shell = useShell();
  return (
    <button type="button" className="ia-link" data-testid="gate-review-link" onClick={() => {
      requestPane(GATEKEEPER_PANE);
      shell.openDialog(SETTINGS_DIALOG);
    }}>
      Review
    </button>
  );
}
