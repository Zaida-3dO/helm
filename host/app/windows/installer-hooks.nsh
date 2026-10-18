; NSIS hooks for the per-user installer (Tauri's template calls these macros).
;
; Tauri's per-user default is "$LOCALAPPDATA\<product>", which is also where Helm keeps its profile
; (database, drafts, staged worker). Program files and user data must not
; share a folder, so the default moves to "$LOCALAPPDATA\Programs\<product>". A folder the user chose
; on the directory page, and the folder of an earlier install (the template restores it), are kept.

!macro NSIS_HOOK_PREINSTALL
  StrCmp $INSTDIR "$LOCALAPPDATA\${PRODUCTNAME}" 0 +4
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${PRODUCTNAME}"
    SetOutPath $INSTDIR
    ; The template already created the old default; drop it if it is empty (never touches data).
    RMDir "$LOCALAPPDATA\${PRODUCTNAME}"
!macroend
