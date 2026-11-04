# Helm editor context (VS Code)

A very small VS Code extension. It writes the file you have open and the text you have selected to a
plain file in Helm's profile folder, so an agent in Helm can read them through its `app_ide_context`
tool. Agents get nothing unless you turn on **Settings > Chat > Agents can read your editor
selection** in Helm; it is off by default and only you can turn it on.

What it writes: one JSON file per VS Code window, `ide-context/<window>.json` inside Helm's profile
folder (`%LOCALAPPDATA%\Helm` on Windows, or the folder `HELM_PROFILE_DIR` names). It holds the
workspace folders, the active file, its language and the current selection (text and 1-based line
range, cut at 16,000 characters). The file is rewritten when the file or selection changes, touched
every 30 seconds while the window is open, and deleted when the window closes. Helm treats a file not
touched for two minutes as gone. Nothing is sent over the network and nothing is read from the editor
beyond this. If Helm's profile folder does not exist yet, the extension writes nothing.

It needs no build step and no packages. To install it, copy this folder to
`%USERPROFILE%\.vscode\extensions\helm.helm-editor-context-0.1.0` (the folder must sit directly under
`.vscode\extensions`), then restart VS Code. To remove it, delete that folder. If your Helm profile is
somewhere else, set `helmEditorContext.folder` in VS Code to the `ide-context` folder inside it.
