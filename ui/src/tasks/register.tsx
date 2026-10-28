// The `tasks` panel kind: the board's Tasks view as one app-wide tab that opens maximised. It reads
// only while shown (see TasksView) and closing the tab closes any open item drawer.

import { IconBoard, IconRefresh } from '../ui/icons.tsx';
import { Button } from '../ui/index.ts';
import { registerPanelKind, panelStore } from '../panel/registry.ts';
import { tasksStoreFor, TASKS_KIND } from './open.ts';
import { TasksView } from './TasksView.tsx';
import './tasks.css';

registerPanelKind({
  kind: TASKS_KIND,
  title: 'Tasks',
  icon: () => <IconBoard size={14} />,
  scope: 'app',
  maxTabs: 1,
  defaultMaximized: true,
  keepMounted: true,
  render: (_tab, ctx) => <TasksView visible={ctx.visible} focused={ctx.focused} />,
  headerActions: () => {
    const api = panelStore.api;
    if (!api) return null;
    return (
      <Button variant="icon" label="Refresh Tasks" data-testid="tasks-refresh" onClick={() => void tasksStoreFor(api).refresh()}>
        <IconRefresh size={14} />
      </Button>
    );
  },
  onClose: () => {
    if (panelStore.api) tasksStoreFor(panelStore.api).closeDrawer();
  },
});
