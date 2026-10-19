// Settings → Runtime: setup status, launch options and new-chat defaults.

import { registerSettingsPane } from '../registry.ts';
import { RuntimePane } from './RuntimePane.tsx';

registerSettingsPane({ id: 'runtime', title: 'Runtime', order: 35, render: () => <RuntimePane /> });
