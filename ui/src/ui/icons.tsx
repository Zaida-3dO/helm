// The kit's icon set: Lucide line icons at the kit defaults (14 px, stroke 2, currentColor).
// Import a named icon from here, never from 'lucide-react' directly, so sizes, stroke and a11y stay
// uniform and the set the app uses is listed in one place. Need one that is missing? Add a line.
//
//   <IconFork />                 14 px, decorative
//   <IconPanelRight size={16} />  a top-bar glyph
//   <IconPin filled />            the pinned state
//   <IconAlert label="Failed" />  an icon that carries meaning on its own
//
// The app's own drawn marks (the wheel mark, the working face, the star) are not Lucide; they live
// in ui/status/.

import {
  AlarmClock,
  Archive,
  Bell,
  BellOff,
  ArchiveRestore,
  ArrowDown,
  ArrowUp,
  ArrowUpRight,
  Bookmark,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  ClipboardList,
  CircleAlert,
  CircleArrowUp,
  CircleCheck,
  CircleHelp,
  Clock,
  Copy,
  CornerDownLeft,
  CornerDownRight,
  CornerLeftUp,
  Ellipsis,
  ExternalLink,
  Eye,
  EyeOff,
  FileDiff,
  FilePlus,
  FileText,
  FoldVertical,
  Flag,
  Folder,
  FolderKanban,
  FolderOpen,
  FolderPlus,
  FolderTree,
  GitBranch,
  GitFork,
  Globe,
  GripVertical,
  Hand,
  Image,
  Info,
  Keyboard,
  LayoutGrid,
  Lightbulb,
  Link,
  ListCollapse,
  LogOut,
  Maximize2,
  MessageSquare,
  MessageSquareDot,
  Mic,
  Minimize2,
  Monitor,
  Moon,
  NotebookPen,
  Palette,
  PanelLeft,
  PanelLeftClose,
  PanelRight,
  Paperclip,
  Pause,
  Pencil,
  Pin,
  PinOff,
  Play,
  Plus,
  Power,
  RefreshCw,
  Rocket,
  RotateCcw,
  RotateCw,
  Search,
  Settings,
  ShieldCheck,
  ShieldOff,
  SlidersHorizontal,
  Sparkles,
  Square,
  SquarePen,
  SquareTerminal,
  Star,
  Sun,
  Terminal,
  Trash,
  TriangleAlert,
  Undo2,
  Wand,
  Workflow,
  X,
  Zap,
} from 'lucide-react';
import { kitIcon } from './iconBase.ts';

export type { IconProps } from './iconBase.ts';
export { ICON_DEFAULT_SIZE, ICON_DEFAULT_STROKE, kitIcon } from './iconBase.ts';

// General.
export const IconX = kitIcon(X, 'x');
export const IconCheck = kitIcon(Check, 'check');
export const IconPlus = kitIcon(Plus, 'plus');
export const IconChevronDown = kitIcon(ChevronDown, 'chevron-down');
export const IconChevronUp = kitIcon(ChevronUp, 'chevron-up');
export const IconChevronLeft = kitIcon(ChevronLeft, 'chevron-left');
export const IconChevronRight = kitIcon(ChevronRight, 'chevron-right');
/** "More" (more-horizontal): the chat menu and every row's overflow. */
export const IconMore = kitIcon(Ellipsis, 'more-horizontal');
export const IconSearch = kitIcon(Search, 'search');
export const IconInfo = kitIcon(Info, 'info');
export const IconAlert = kitIcon(CircleAlert, 'circle-alert');
export const IconWarning = kitIcon(TriangleAlert, 'triangle-alert');
export const IconSuccess = kitIcon(CircleCheck, 'circle-check');
export const IconHelp = kitIcon(CircleHelp, 'circle-help');
export const IconExternalLink = kitIcon(ExternalLink, 'external-link');
export const IconArrowUpRight = kitIcon(ArrowUpRight, 'arrow-up-right');
export const IconLink = kitIcon(Link, 'link');
export const IconTrash = kitIcon(Trash, 'trash');
export const IconGrip = kitIcon(GripVertical, 'grip-vertical');
export const IconSliders = kitIcon(SlidersHorizontal, 'sliders-horizontal');

// Sidebar.
export const IconPanelLeft = kitIcon(PanelLeft, 'panel-left');
/** The new-chat glyph. */
export const IconNewChat = kitIcon(SquarePen, 'square-pen');
/** Needs attention. */
export const IconFlag = kitIcon(Flag, 'flag');
/** Swarm. */
export const IconGrid = kitIcon(LayoutGrid, 'layout-grid');
export const IconFolder = kitIcon(Folder, 'folder');
export const IconFolderOpen = kitIcon(FolderOpen, 'folder-open');
export const IconFolderPlus = kitIcon(FolderPlus, 'folder-plus');
export const IconFolderTree = kitIcon(FolderTree, 'folder-tree');
export const IconWand = kitIcon(Wand, 'wand');
export const IconChat = kitIcon(MessageSquare, 'message-square');

// Top bar and the chat menu.
/** Fork: the branch glyph. */
export const IconFork = kitIcon(GitBranch, 'git-branch');
/** The git-fork glyph, for places that mean a fork in history rather than "fork this chat". */
export const IconGitFork = kitIcon(GitFork, 'git-fork');
export const IconPin = kitIcon(Pin, 'pin');
export const IconUnpin = kitIcon(PinOff, 'pin-off');
export const IconArchive = kitIcon(Archive, 'archive');
/** Restore an archived chat (rotate-ccw). */
export const IconRestore = kitIcon(RotateCcw, 'rotate-ccw');
export const IconUnarchive = kitIcon(ArchiveRestore, 'archive-restore');
export const IconPanelRight = kitIcon(PanelRight, 'panel-right');
export const IconChatView = kitIcon(ListCollapse, 'list-collapse');
export const IconRename = kitIcon(Pencil, 'pencil');
export const IconUpdate = kitIcon(CircleArrowUp, 'circle-arrow-up');
export const IconRefresh = kitIcon(RefreshCw, 'refresh-cw');
export const IconRestart = kitIcon(RotateCw, 'rotate-cw');

// The top-bar action row and its actions.
/** The action row's overflow ("Actions"). Distinct from IconMore, which is the chat menu. */
export const IconActions = kitIcon(Zap, 'zap');
/** Needs me. */
export const IconHand = kitIcon(Hand, 'hand');
/** Ship it. */
export const IconRocket = kitIcon(Rocket, 'rocket');
/** Close session. */
export const IconPower = kitIcon(Power, 'power');
/** Explain plainly. */
export const IconExplain = kitIcon(Lightbulb, 'lightbulb');
export const IconSparkles = kitIcon(Sparkles, 'sparkles');

// Composer.
/** Send: the arrow on the send button. */
export const IconSend = kitIcon(ArrowUp, 'arrow-up');
/** Stop: draw it `filled` at 12 px. */
export const IconStop = kitIcon(Square, 'square');
export const IconEye = kitIcon(Eye, 'eye');
export const IconEyeOff = kitIcon(EyeOff, 'eye-off');
export const IconAttach = kitIcon(Paperclip, 'paperclip');
export const IconStar = kitIcon(Star, 'star');
/** AFK. */
export const IconMoon = kitIcon(Moon, 'moon');
/** The orchestrator switch. */
export const IconWorkflow = kitIcon(Workflow, 'workflow');
/** The permission chip. */
export const IconShield = kitIcon(ShieldCheck, 'shield-check');
/** The composer's permission chip: bypass (no prompts) and plan mode. */
export const IconShieldOff = kitIcon(ShieldOff, 'shield-off');
export const IconPlan = kitIcon(ClipboardList, 'clipboard-list');
export const IconMic = kitIcon(Mic, 'mic');
export const IconImage = kitIcon(Image, 'image');
export const IconEnter = kitIcon(CornerDownLeft, 'corner-down-left');
export const IconPause = kitIcon(Pause, 'pause');
export const IconPlay = kitIcon(Play, 'play');

// Transcript actions and tool rows.
export const IconCopy = kitIcon(Copy, 'copy');
/** Save a message. */
export const IconBookmark = kitIcon(Bookmark, 'bookmark');
/** Remind me. */
export const IconRemind = kitIcon(AlarmClock, 'alarm-clock');
/** The user-bubble edit pencil. */
export const IconEdit = kitIcon(Pencil, 'pencil');
export const IconUndo = kitIcon(Undo2, 'undo-2');
export const IconClock = kitIcon(Clock, 'clock');
export const IconTerminal = kitIcon(Terminal, 'terminal');
export const IconScrollDown = kitIcon(ArrowDown, 'arrow-down');

// Side panel.
export const IconFileText = kitIcon(FileText, 'file-text');
export const IconFileDiff = kitIcon(FileDiff, 'file-diff');
export const IconFilePlus = kitIcon(FilePlus, 'file-plus');
export const IconNote = kitIcon(NotebookPen, 'notebook-pen');
export const IconMaximize = kitIcon(Maximize2, 'maximize-2');
export const IconMinimize = kitIcon(Minimize2, 'minimize-2');
export const IconBrowser = kitIcon(Globe, 'globe');
export const IconShell = kitIcon(SquareTerminal, 'square-terminal');

// Sidebar rows and menus.
/** The muted mark (bell-off 11) and "Mute chat". */
export const IconMute = kitIcon(BellOff, 'bell-off');
export const IconUnmute = kitIcon(Bell, 'bell');
export const IconUnread = kitIcon(MessageSquareDot, 'message-square-dot');
/** "Nest under…" and "Move to top level". */
export const IconNest = kitIcon(CornerDownRight, 'corner-down-right');
export const IconUnnest = kitIcon(CornerLeftUp, 'corner-left-up');
/** A board project's default icon. */
export const IconBoard = kitIcon(FolderKanban, 'folder-kanban');
/** "Hide sidebar" in the sidebar's title strip (panel-close 16). */
export const IconSidebarClose = kitIcon(PanelLeftClose, 'panel-left-close');
/** "Compact context" in the chat menu. */
export const IconCompact = kitIcon(FoldVertical, 'fold-vertical');

// Sidebar footer and Settings.
export const IconSettings = kitIcon(Settings, 'settings');
export const IconQuit = kitIcon(LogOut, 'log-out');
export const IconSun = kitIcon(Sun, 'sun');
export const IconMonitor = kitIcon(Monitor, 'monitor');
export const IconPalette = kitIcon(Palette, 'palette');
export const IconKeyboard = kitIcon(Keyboard, 'keyboard');
