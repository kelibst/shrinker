; Inno Setup script for the Windows installer (Shrinker-Setup-<version>.exe).
;
;   iscc /DVersion=0.2.0 packaging\windows\shrinker.iss
;
; Installs for the current user only (no admin prompt) into
; %LOCALAPPDATA%\Programs\Shrinker, adds Start menu / desktop / "Send to"
; shortcuts, can add shrinker to PATH, and offers to install ffmpeg with winget.

#define AppName "Shrinker"
#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\shrinker.exe"
#endif

[Setup]
AppId={{8E6C2B1A-5D2F-4C77-9B1E-3A7F0C2D9E41}
AppName={#AppName}
AppVersion={#Version}
AppVerName={#AppName} {#Version}
AppPublisher=Keli Booster
AppPublisherURL=https://github.com/kelibst/shrinker
AppSupportURL=https://github.com/kelibst/shrinker/issues
DefaultDirName={autopf}\Shrinker
DefaultGroupName=Shrinker
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\..\dist
OutputBaseFilename=Shrinker-Setup-{#Version}
SetupIconFile=..\icons\shrinker.ico
UninstallDisplayIcon={app}\shrinker.ico
UninstallDisplayName={#AppName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile=..\..\LICENSE

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "sendto"; Description: "Add Shrinker to the right-click ""Send to"" menu (for folders and files)"; GroupDescription: "Shortcuts:"
Name: "addpath"; Description: "Let me type ""shrinker"" in a terminal (adds it to PATH)"; GroupDescription: "Terminal:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\icons\shrinker.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Shrinker"; Filename: "{app}\shrinker.exe"; IconFilename: "{app}\shrinker.ico"; Comment: "Make videos and images smaller"
Name: "{group}\Uninstall Shrinker"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Shrinker"; Filename: "{app}\shrinker.exe"; IconFilename: "{app}\shrinker.ico"; Tasks: desktopicon
Name: "{usersendto}\Shrinker"; Filename: "{app}\shrinker.exe"; IconFilename: "{app}\shrinker.ico"; Tasks: sendto

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addpath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{cmd}"; Parameters: "/c winget install -e --id Gyan.FFmpeg --accept-source-agreements --accept-package-agreements"; Description: "Install ffmpeg now (Shrinker needs it; downloads about 100 MB)"; StatusMsg: "Installing ffmpeg with winget…"; Flags: postinstall waituntilterminated; Check: NeedsFfmpeg
Filename: "{app}\shrinker.exe"; Description: "Open Shrinker"; Flags: postinstall nowait skipifsilent

[Code]
function NeedsAddPath(Dir: string): Boolean;
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') = 0;
end;

function CommandWorks(Cmd: string): Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{cmd}'), '/c ' + Cmd + ' >nul 2>nul', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

// Offer the winget step only when ffmpeg is missing and winget is there to fetch it.
function NeedsFfmpeg: Boolean;
begin
  Result := (not CommandWorks('where ffmpeg'))
    and (not FileExists(ExpandConstant('{localappdata}\Microsoft\WinGet\Links\ffmpeg.exe')))
    and CommandWorks('where winget');
end;

// Take Shrinker back off PATH when it is uninstalled.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths, Dir: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then exit;
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Paths) then exit;
  Dir := ';' + ExpandConstant('{app}');
  P := Pos(Uppercase(Dir), Uppercase(Paths));
  if P > 0 then
  begin
    Delete(Paths, P, Length(Dir));
    RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Paths);
  end;
end;
