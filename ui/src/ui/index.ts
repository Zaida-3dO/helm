// The shared UI primitives kit. Every area imports these and writes
// no buttons, dialogs, menus, pickers, tabs, tooltips or toasts of its own. Styles: ui/ui.css
// (loaded once by main.tsx); tokens: styles.css.
//
// Escape: register through `useLayer` (layers.ts), never a keydown handler.
// Shortcuts: register through `useBinding` / `keys.register` (keys.ts), never a keydown handler.

export { Button, Checkbox, Chip, Icon, Segmented, SettingRow, TextArea, TextInput } from './controls.tsx';
export type { ButtonProps, ButtonVariant, CheckboxProps, SegmentedOption, TextInputProps } from './controls.tsx';
export { AnchoredDialog, Dialog } from './Dialog.tsx';
export type { AnchoredDialogProps, DialogProps } from './Dialog.tsx';
export { ContextMenu } from './Menu.tsx';
export type { ContextMenuProps, MenuItem } from './Menu.tsx';
export { Picker, filterOptions } from './Picker.tsx';
export type { PickerOption, PickerProps } from './Picker.tsx';
export { PillTabs, Toast, Tooltip } from './misc.tsx';
export type { Tab, ToastProps } from './misc.tsx';
export { ToastSlotView } from './ToastSlot.tsx';
export { toast, ToastSlot } from './toasts.ts';
export type { LiveToast, ToastKind, ToastSpec } from './toasts.ts';
export { layers, LayerStack, LAYER_ORDER } from './layers.ts';
export type { Layer, LayerKind } from './layers.ts';
export { keys, KeyRegistry, RESERVED, chordOf, hintLabel, isAssignable, normaliseChord, KEYBINDINGS_PREF } from './keys.ts';
export type { Binding, BindingScope, Conflict } from './keys.ts';
export { useBinding, useChord, useHint, useLayer } from './hooks.ts';
export { placeAtPoint, placeBelow, nextIndex } from './place.ts';
export type { Rect } from './place.ts';
