Unicode true
RequestExecutionLevel user
ManifestDPIAware true
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\NiceEnv"
!define WEBVIEW_KEY "Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_ABORTWARNING
Name "NiceEnv"
OutFile "${OUTPUT}"
InstallDir "$LOCALAPPDATA\NiceEnv"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "NiceEnv"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "NiceEnv Installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Copyright nsmao-com"
SetCompressor /SOLID lzma
ShowInstDetails show
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"
!insertmacro MUI_LANGUAGE "English"

Function .onInit
  SetShellVarContext current
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "NiceEnv requires 64-bit Windows."
    Abort
  ${EndIf}
FunctionEnd

!macro RequireClosed file
  IfFileExists "$INSTDIR\${file}" 0 +7
  ClearErrors
  FileOpen $0 "$INSTDIR\${file}" a
  IfErrors 0 +3
  MessageBox MB_ICONSTOP "Please close NiceEnv and its command-line tools before continuing: ${file}"
  Abort
  FileClose $0
!macroend

Section "NiceEnv"
  SetShellVarContext current
  !insertmacro RequireClosed "NiceEnv.exe"
  !insertmacro RequireClosed "niceservbay.exe"
  !insertmacro RequireClosed "nsbctl.exe"
  !insertmacro RequireClosed "nsb-mcp.exe"
  ; Edge installs may be either per-user or machine-wide, in either registry view.
  SetRegView 32
  ReadRegStr $1 HKCU "${WEBVIEW_KEY}" "pv"
  ${If} $1 == ""
    ReadRegStr $1 HKLM "${WEBVIEW_KEY}" "pv"
  ${EndIf}
  ${If} $1 == ""
    SetRegView 64
    ReadRegStr $1 HKLM "${WEBVIEW_KEY}" "pv"
    SetRegView 32
  ${EndIf}
  ${If} $1 == ""
  ${OrIf} $1 == "0.0.0.0"
    InitPluginsDir
    SetOutPath "$PLUGINSDIR"
    File /oname=WebView2Setup.exe "${BOOTSTRAPPER}"
    ExecWait '"$PLUGINSDIR\WebView2Setup.exe" /silent /install' $2
    ${If} $2 != 0
    ${AndIf} $2 != 3010
      MessageBox MB_ICONSTOP "WebView2 installation failed ($2). Install Microsoft Edge WebView2 Runtime and retry."
      Abort
    ${EndIf}
  ${EndIf}
  SetOutPath "$INSTDIR"
  SetOverwrite on
  File "${STAGE}\NiceEnv.exe"
  File "${STAGE}\nsbctl.exe"
  File "${STAGE}\nsb-mcp.exe"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\NiceEnv.lnk" "$INSTDIR\NiceEnv.exe"
  CreateShortcut "$DESKTOP\NiceEnv.lnk" "$INSTDIR\NiceEnv.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "NiceEnv"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "nsmao-com"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\NiceEnv.exe"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\uninstall.exe$\"'
  WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '$\"$INSTDIR\uninstall.exe$\" /S'
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "EstimatedSize" ${INSTALL_SIZE}
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  SetShellVarContext current
  !insertmacro RequireClosed "NiceEnv.exe"
  !insertmacro RequireClosed "nsbctl.exe"
  !insertmacro RequireClosed "nsb-mcp.exe"
  Delete "$INSTDIR\NiceEnv.exe"
  Delete "$INSTDIR\nsbctl.exe"
  Delete "$INSTDIR\nsb-mcp.exe"
  Delete "$INSTDIR\uninstall.exe"
  Delete "$SMPROGRAMS\NiceEnv.lnk"
  Delete "$DESKTOP\NiceEnv.lnk"
  DeleteRegKey HKCU "${UNINSTALL_KEY}"
  ; Never recurse: nsb-data, sites, runtimes, databases and custom files survive.
  RMDir "$INSTDIR"
SectionEnd
