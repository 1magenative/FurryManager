; FurryManager - NSIS installer hooks.
;
; The app keeps its settings in %APPDATA%\FurryManager (config.json: library
; path, PIN hash, server token, models_ready marker). Tauri's stock uninstaller
; only wipes %APPDATA%\<bundle identifier>, so that folder would survive even
; when the user ticks "delete application data". For a privacy-oriented app
; that is wrong: the PIN hash and the library path must go with it.
;
; The library folder itself (store/, thumbs/, library.db) is chosen by the user
; and lives outside both $INSTDIR and %APPDATA% - it is NEVER touched here.
;
; ASCII only on purpose: NSIS reads .nsh without BOM as ANSI.

!macro NSIS_HOOK_POSTUNINSTALL
  ; $DeleteAppDataCheckboxState - "delete application data" checkbox
  ; $UpdateMode - set when the uninstaller runs as part of an in-place upgrade
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    SetShellVarContext current
    RMDir /r "$APPDATA\FurryManager"
  ${EndIf}
!macroend
