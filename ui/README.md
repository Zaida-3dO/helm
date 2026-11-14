# Helm's renderer

The React + TypeScript renderer, loaded by Tauri. How to build, test and preview it is in the repository's
`CLAUDE.md`; the stable end-to-end hooks are in `TESTIDS.md`.

## One-line controls when room runs out

A label on a one-line control (a chip, a tab, a button, a title) never wraps and is never cut off hard: it ends
in an ellipsis and carries its full text as a tooltip. A group of controls gives up room in one order, its
secondary words first, then its labels down to icons, and a control is never left as a bare caret. Buttons that
belong together, such as Deny and Allow, stay side by side on one line.
