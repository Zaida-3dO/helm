// Settings → Profiles: one registerSettingsPane line.

import { createElement } from 'react';
import { registerSettingsPane } from '../registry.ts';
import { ProfilesPane } from './ProfilesPane.tsx';

registerSettingsPane({ id: 'profiles', title: 'Profiles', order: 41, render: () => createElement(ProfilesPane) });
