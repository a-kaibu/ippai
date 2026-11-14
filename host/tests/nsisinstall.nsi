; host/tests/nsisinstall.c's installer (makensis; Japanese Modern UI, LZMA solid): files,
; a registry value, an uninstaller and its Uninstall key
Unicode true
SetCompressor /SOLID lzma
!include "MUI2.nsh"
Name "masu nsis test"
OutFile "nsisinstall-setup.exe"
InstallDir "$PROGRAMFILES\NsisTest"
RequestExecutionLevel admin
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Japanese"
Section "Main"
  SetOutPath "$INSTDIR"
  File a.txt
  SetOutPath "$INSTDIR\sub"
  File b.dat
  WriteRegStr HKCU "Software\masu\nsistest" "path" "$INSTDIR"
  WriteUninstaller "$INSTDIR\uninst.exe"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\NsisTest" "UninstallString" '"$INSTDIR\uninst.exe"'
SectionEnd
Section "Uninstall"
  Delete "$INSTDIR\a.txt"
  Delete "$INSTDIR\sub\b.dat"
  RMDir "$INSTDIR\sub"
  Delete "$INSTDIR\uninst.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKCU "Software\masu\nsistest"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\NsisTest"
SectionEnd
