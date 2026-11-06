; Inno Setup script for the Plexi installer.
; Built by .github/workflows/release.yml:
;   ISCC /DAppVersion=0.3.0 /DSourceExe=<repo>\target\release\plexi.exe /O<repo>\dist installer\plexi.iss
; Installs per user by default (no admin prompt); the first page offers an all-users install.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\plexi.exe"
#endif

[Setup]
AppId={{8B4F5C2E-6E1A-4C51-9D7F-2B3A9C0E1F44}
AppName=Plexi
AppVersion={#AppVersion}
AppVerName=Plexi {#AppVersion}
AppPublisher=Hayden Harms
AppPublisherURL=https://github.com/HaydenHarms/plexi
AppSupportURL=https://github.com/HaydenHarms/plexi/issues
AppUpdatesURL=https://github.com/HaydenHarms/plexi/releases
DefaultDirName={autopf}\Plexi
DefaultGroupName=Plexi
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputBaseFilename=Plexi-{#AppVersion}-setup
SetupIconFile=..\assets\plexi.ico
UninstallDisplayIcon={app}\plexi.exe
UninstallDisplayName=Plexi
LicenseFile=..\LICENSE
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesAssociations=yes
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "assoc"; Description: "Open .plexi notebooks with Plexi"; GroupDescription: "Files:"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "startup"; Description: "Start Plexi with Windows, ready to draw on screen (Ctrl+Alt+D)"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "plexi.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Plexi"; Filename: "{app}\plexi.exe"
Name: "{autoprograms}\Plexi (draw on screen)"; Filename: "{app}\plexi.exe"; Parameters: "--draw"
Name: "{autodesktop}\Plexi"; Filename: "{app}\plexi.exe"; Tasks: desktopicon

[Registry]
Root: HKA; Subkey: "Software\Classes\.plexi"; ValueType: string; ValueName: ""; ValueData: "Plexi.Notebook"; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.plexi"; ValueType: string; ValueName: "Content Type"; ValueData: "application/x-plexi"; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Plexi.Notebook"; ValueType: string; ValueName: ""; ValueData: "Plexi notebook"; Flags: uninsdeletekey; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Plexi.Notebook\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\plexi.exe,0"; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Plexi.Notebook\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\plexi.exe"" ""%1"""; Tasks: assoc
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Plexi"; ValueData: """{app}\plexi.exe"" --background"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\plexi.exe"; Description: "{cm:LaunchProgram,Plexi}"; Flags: nowait postinstall skipifsilent
; Plexi's updater runs this installer silently with /relaunch=1 and expects Plexi back.
Filename: "{app}\plexi.exe"; Flags: nowait; Check: ShouldRelaunch

[UninstallRun]
Filename: "{cmd}"; Parameters: "/C taskkill /IM plexi.exe /F"; Flags: runhidden; RunOnceId: "StopPlexi"

[Code]
function ShouldRelaunch: Boolean;
begin
  Result := ExpandConstant('{param:relaunch|0}') = '1';
end;
