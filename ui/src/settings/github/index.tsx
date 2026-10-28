// Settings → GitHub: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { GitHubPane } from './GitHubPane.tsx';

registerSettingsPane({ id: 'github', title: 'GitHub', order: 44, render: () => createElement(GitHubPane) });
