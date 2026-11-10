// Profiles: who uses this Helm. The footer chip and switcher, the household block, the profile view
// and the sidebar filter. Settings → Profiles registers itself from ../settings/profiles.

export { ProfileChip, PROFILES_PANE } from './ProfileChip.tsx';
export { HouseholdSection } from './HouseholdSection.tsx';
export { ProfileView } from './ProfileView.tsx';
export { SharingSwitches } from './SharingSwitches.tsx';
export { useProfiles } from './hooks.ts';
export { profileChoices, moveChatToProfile, type ChatProfileChoice } from './menu.ts';
export { rowsForProfile } from './model.ts';
export { profilesStoreFor } from './store.ts';
