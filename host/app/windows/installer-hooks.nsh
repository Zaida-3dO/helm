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
; The one exception: when the profile holds diagnostics\update-pending.json, this setup was started
; by Helm's own "restart to update" (or the app_update agent tool) — Helm has already closed
; itself, and the launcher is what brings the new Helm up when this setup ends, so it is left
; running. The marker lives in the default profile only; a setup aimed at another profile's Helm
; finds no marker and ends its launcher as always.

; End the default profile's launcher: the one whose process id it noted in its profile. Other
; profiles' launchers are left alone. An id that now belongs to some other program is passed over
; by the image-name filter, and a file that holds no number ends nothing. $0 and $1 are kept.
!macro HELM_END_LAUNCHER
  Push $0
  Push $1
  ClearErrors
  FileOpen $0 "$LOCALAPPDATA\${PRODUCTNAME}\launcher\launcher.pid" r
  IfErrors helm_launcher_done
  FileRead $0 $1
  FileClose $0
  IntOp $1 $1 + 0
  IntCmp $1 0 helm_launcher_done helm_launcher_done
  nsExec::Exec '"$SYSDIR\taskkill.exe" /F /PID $1 /FI "IMAGENAME eq helm-launcher.exe"'
  Pop $0
  helm_launcher_done:
  Pop $1
  Pop $0
!macroend

; Remove every profile's logon entry for the launcher: the default profile's "Helm launcher" and
; each other profile's "Helm launcher (<id>)". $0, $1 and $2 are kept.
!macro HELM_REMOVE_RUN_VALUES
  Push $0
  Push $1
  Push $2
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Helm launcher"
  StrCpy $0 0
  helm_run_next:
    ClearErrors
    EnumRegValue $1 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" $0
    IfErrors helm_run_done
    StrCmp $1 "" helm_run_done
    StrCpy $2 $1 15
    StrCmp $2 "Helm launcher (" 0 helm_run_skip
      DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" $1
      ; The next value has moved into this place.
      Goto helm_run_next
    helm_run_skip:
    IntOp $0 $0 + 1
    Goto helm_run_next
  helm_run_done:
  Pop $2
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  StrCmp $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}" 0 +4
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
    SetOutPath $INSTDIR
    ; The template already created the old default; drop it if it is empty (never touches data).
    RMDir "$LOCALAPPDATA\${PRODUCTNAME}"
  ; A run that quit to run this setup left its pending-update marker: the launcher is that
  ; update's relauncher, so it stays alive and starts the new Helm when this setup ends.
  ; Without the marker (a normal run or install) the launcher must not restart Helm mid-update.
  IfFileExists "$LOCALAPPDATA\${PRODUCTNAME}\diagnostics\update-pending.json" helm_keep_launcher 0
    !insertmacro HELM_END_LAUNCHER
  helm_keep_launcher:
!macroend

; Uninstalling ends the launcher and removes every profile's logon entry for it; its copy stays
; with the profile, which an uninstall never touches.
!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro HELM_END_LAUNCHER
  !insertmacro HELM_REMOVE_RUN_VALUES
!macroend
