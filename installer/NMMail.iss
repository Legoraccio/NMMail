; Inno Setup script for a per-user NM Mail installer (no admin rights).
; Build first:   .\scripts\build.ps1
; Then compile:  iscc installer\NMMail.iss   (Inno Setup 6: winget install JRSoftware.InnoSetup)

#define AppName "NM Mail"
#define AppVersion "0.1.0"
#define Arch "x64"

[Setup]
AppId={{6B1E9C52-4F1A-4E7B-9C3D-2A7D9E1F0B11}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=NM Mail
DefaultDirName={localappdata}\Programs\NMMail
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=NMMail-Setup-{#AppVersion}-{#Arch}
SetupIconFile=..\assets\icon\nmmail.ico
UninstallDisplayIcon={app}\NMMail.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=force

[Languages]
Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "..\dist\NMMail-{#Arch}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\NMMail.exe"; WorkingDir: "{app}"

[Run]
Filename: "{app}\nmmaild.exe"; Parameters: "--register"; Flags: runhidden
Filename: "{app}\nmmaild.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall

[UninstallRun]
Filename: "{cmd}"; Parameters: "/c taskkill /im nmmaild.exe /f & taskkill /im NMMail.exe /f"; Flags: runhidden; RunOnceId: "KillNMMail"
Filename: "{app}\nmmaild.exe"; Parameters: "--unregister"; Flags: runhidden; RunOnceId: "Unregister"
