; NSIS hooks for the per-user installer (Tauri's template calls these macros).
;
; Tauri's per-user default is "$LOCALAPPDATA\<product>", which is also where Helm keeps its profile
; (database, drafts, staged worker). Program files and user data must not
; share a folder, so the default moves to "$LOCALAPPDATA\Programs\<product>". A folder the user chose
; on the directory page, and the folder of an earlier install (the template restores it), are kept.
;
; The always-on launcher (helm-launcher.exe, run from a copy in the profile) restarts Helm when Helm
; ends without closing. The template ends a running Helm before it writes the new files, which
; looks exactly like that, so the launcher is ended first: otherwise it could start Helm again in
; the middle of the install. Helm starts it again (from a fresh copy) when it next starts.

; End every helm-launcher.exe this user runs (a per-user installer can end no one else's).
; $0 is kept as it was.
!macro HELM_END_LAUNCHER
  Push $0
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /IM helm-launcher.exe'
  Pop $0
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  StrCmp $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}" 0 +4
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
    SetOutPath $INSTDIR
    ; The template already created the old default; drop it if it is empty (never touches data).
    RMDir "$LOCALAPPDATA\${PRODUCTNAME}"
  !insertmacro HELM_END_LAUNCHER
!macroend

; Uninstalling ends the launcher and removes its logon entry; its copy stays with the profile,
; which an uninstall never touches.
!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro HELM_END_LAUNCHER
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Helm launcher"
!macroend
