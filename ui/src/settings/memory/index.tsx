// Settings → Memory: one registerSettingsPane line. It sits after Agents (49) and before the
// governor and schedules, with the other settings about what chats can reach.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { MemoryPane } from './MemoryPane.tsx';

registerSettingsPane({ id: 'memory', title: 'Memory', order: 51, render: () => createElement(MemoryPane) });
