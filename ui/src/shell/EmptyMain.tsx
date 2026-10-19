// The main pane with no chat open: a centred heading, a one-line tagline and one
// primary button that opens a project. The new-chat page (NewChatHost, mounted
// beside this in App.tsx) covers it while a draft is open.

import { openAddProject } from '../newchat/index.ts';
import { Button } from '../ui/index.ts';
import { IconArrowUpRight, IconFolderOpen } from '../ui/icons.tsx';

export const EMPTY_HEADING = 'What are we building?';
export const EMPTY_TAGLINE = 'Point your agents at a project, then steer the work from here.';

export function EmptyMain({ onOpenProject = openAddProject }: { onOpenProject?: () => void }) {
  return (
    <section className="empty-main" data-testid="empty-main" aria-labelledby="empty-main-heading">
      <h1 id="empty-main-heading" className="empty-main-heading">
        {EMPTY_HEADING}
      </h1>
      <p className="empty-main-tagline">{EMPTY_TAGLINE}</p>
      <Button variant="primary" className="empty-main-open" data-testid="empty-open-project" onClick={() => onOpenProject()}>
        <IconFolderOpen size={16} />
        <span>Open a project</span>
        <IconArrowUpRight size={15} />
      </Button>
    </section>
  );
}
