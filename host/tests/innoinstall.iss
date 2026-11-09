; An Inno Setup installer for host/tests/innoinstall.c, compiled by tests/host-tests.sh with
; Inno Setup's own ISCC running on sake: two files, a Start menu shortcut, a registry value
; removed with the program, and the uninstaller.
[Setup]
AppName=InnoTest
AppVersion=1.0
AppPublisher=ippai
DefaultDirName={autopf}\InnoTest
DefaultGroupName=InnoTest
DisableProgramGroupPage=yes
PrivilegesRequired=admin
OutputDir=out
OutputBaseFilename=innoinstall-setup
Compression=lzma2
SolidCompression=yes

[Files]
Source: "a.txt"; DestDir: "{app}"
Source: "b.dat"; DestDir: "{app}\sub"

[Icons]
Name: "{group}\InnoTest"; Filename: "{app}\a.txt"; Parameters: "/x"; WorkingDir: "{app}"; Comment: "the test file"

[Registry]
Root: HKCU; Subkey: "Software\ippai\innotest"; ValueType: string; ValueName: "path"; ValueData: "{app}"; Flags: uninsdeletekey
