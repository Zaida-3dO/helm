// Settings → Skills & rules and Settings → Agents: one registerSettingsPane line each.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { AgentsPane } from './AgentsPane.tsx';
import { KnowledgePane } from './KnowledgePane.tsx';

registerSettingsPane({ id: 'skills', title: 'Skills & rules', order: 47, render: () => createElement(KnowledgePane) });
registerSettingsPane({ id: 'agents', title: 'Agents', order: 49, render: () => createElement(AgentsPane) });
