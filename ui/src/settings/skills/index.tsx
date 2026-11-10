// Settings → Skills and Settings → Agents: one registerSettingsPane line each.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { AgentsPane } from './AgentsPane.tsx';
import { SkillsPane } from './SkillsPane.tsx';

registerSettingsPane({ id: 'skills', title: 'Skills', order: 47, render: () => createElement(SkillsPane) });
registerSettingsPane({ id: 'agents', title: 'Agents', order: 49, render: () => createElement(AgentsPane) });
