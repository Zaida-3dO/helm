// Settings → Skills: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { SkillsPane } from './SkillsPane.tsx';

registerSettingsPane({ id: 'skills', title: 'Skills', order: 47, render: () => createElement(SkillsPane) });
