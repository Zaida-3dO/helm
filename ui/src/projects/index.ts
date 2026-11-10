// The projects area's public surface: a project's icon wherever it is drawn, and the icon picker.
//
//   <ProjectIcon project size />   emoji, library icon, picture or the default glyph
//   openIconPicker(projectId)      a project menu's Change icon
//   <IconPickerHost />             mounted once by the app frame

export { IconPickerHost, ProjectIcon, openIconPicker } from './ProjectIcon.tsx';
export type { ProjectIconSource } from './ProjectIcon.tsx';
