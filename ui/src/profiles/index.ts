// Profiles: who uses this Helm. The sidebar's profile item and the profile screen, the first-run
// question, and the sidebar filter. Settings → Profiles registers itself from ../settings/profiles.

export { ProfileItem, PROFILES_PANE } from './ProfileItem.tsx';
export { ProfileScreen } from './ProfileScreen.tsx';
export { SharingSwitches } from './SharingSwitches.tsx';
export { useProfiles } from './hooks.ts';
export { profileChoices, shareChoices, moveChatToProfile, shareChatWith, type ChatProfileChoice, type ChatShareChoice } from './menu.ts';
export { rowsForProfile, sharedFrom } from './model.ts';
export { profilesStoreFor } from './store.ts';
