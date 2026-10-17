// Settings frame entry. Each area imports `registerSettingsPane` from './registry.ts' in its own
// pane module and add one side-effect import line below.
export { registerSettingsPane } from './registry.ts';
export { SettingsDialog, SETTINGS_DIALOG } from './SettingsDialog.tsx';
