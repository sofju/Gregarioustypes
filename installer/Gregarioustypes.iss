; Gregarioustypes installer (Inno Setup 7). Built by scripts\package.ps1, which
; passes /DAppVersion, /DRoot (the repository) and /DIcon.
; Per-user install, no administrator rights: %LOCALAPPDATA%\Programs\Gregarioustypes.
; LibreOffice, Blender and Ghostscript are optional components. The licence texts
; (target\licenses, from scripts\collect-licenses.py) go to {app}\licenses.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif
#ifndef Root
  #define Root ".."
#endif

[Setup]
AppId={{6E1F3B5A-2C47-4D1B-9B8E-6F6A4C3D2E10}
AppName=Gregarioustypes
AppVersion={#AppVersion}
AppVerName=Gregarioustypes {#AppVersion}
AppPublisher=sofju
AppPublisherURL=https://github.com/sofju/Gregarioustypes
AppSupportURL=https://github.com/sofju/Gregarioustypes/issues
AppUpdatesURL=https://github.com/sofju/Gregarioustypes/releases
AppCopyright=Copyright (C) 2026 sofju. GNU GPL v3 or later
VersionInfoDescription=Gregarioustypes setup
DefaultDirName={localappdata}\Programs\Gregarioustypes
DefaultGroupName=Gregarioustypes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir={#Root}\target\installer
OutputBaseFilename=Gregarioustypes-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
LZMANumBlockThreads=8
WizardStyle=modern
UninstallDisplayIcon={app}\Gregarioustypes.exe
UninstallDisplayName=Gregarioustypes
#ifdef Icon
SetupIconFile={#Icon}
#endif
; The app is closed by --quit (below), not by the Restart Manager.
CloseApplications=no

[Types]
Name: "full"; Description: "Everything (previews for every supported format)"
Name: "compact"; Description: "Without LibreOffice and Blender (Office files and .blend show their saved previews only)"
Name: "custom"; Description: "Choose components"; Flags: iscustom

[Components]
Name: "main"; Description: "Previewer (images, video, audio, PDF, archives, web, 3D, text)"; Types: full compact custom; Flags: fixed
Name: "postscript"; Description: "PostScript and EPS (Ghostscript, 90 MB)"; Types: full compact
Name: "office"; Description: "Word, PowerPoint and OpenDocument files (LibreOffice, 1.6 GB)"; Types: full
Name: "blender"; Description: "Blender files, and USD files three.js can't read (Blender, 930 MB)"; Types: full

[Tasks]
Name: "autostart"; Description: "Start with Windows"; GroupDescription: "Options:"

[Files]
Source: "{#Root}\target\release\Gregarioustypes.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#Root}\target\release\gt_dialoghook.dll"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#Root}\target\release\gt_dialoghook32.exe"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#Root}\target\release\gt_dialoghook32.dll"; DestDir: "{app}"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\mpv\libmpv-2.dll"; DestDir: "{app}\deps\mpv"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\mediainfo\MediaInfo.dll"; DestDir: "{app}\deps\mediainfo"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\pdfium\bin\pdfium.dll"; DestDir: "{app}\deps\pdfium\bin"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\pdfium\LICENSE"; DestDir: "{app}\deps\pdfium"; Components: main; Flags: ignoreversion
; The app's own 7z.dll (built without the RAR decoders), and the official 7-Zip
; program that extracts files from RAR archives in a separate process.
Source: "{#Root}\deps\7zip-lgpl\*"; DestDir: "{app}\deps\7zip-lgpl"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\7zip\7z.exe"; DestDir: "{app}\deps\7zip"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\7zip\7z.dll"; DestDir: "{app}\deps\7zip"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\7zip\License.txt"; DestDir: "{app}\deps\7zip"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\soundfont\*"; DestDir: "{app}\deps\soundfont"; Components: main; Flags: ignoreversion
Source: "{#Root}\deps\ruffle\*"; DestDir: "{app}\deps\ruffle"; Components: main; Flags: ignoreversion recursesubdirs
Source: "{#Root}\deps\three\*"; DestDir: "{app}\deps\three"; Components: main; Flags: ignoreversion recursesubdirs
Source: "{#Root}\deps\ghostscript\*"; DestDir: "{app}\deps\ghostscript"; Components: postscript; Flags: ignoreversion recursesubdirs; Excludes: "doc\*,examples\*,vcredist_x64.exe,uninstgs.exe.nsis"
Source: "{#Root}\deps\libreoffice\*"; DestDir: "{app}\deps\libreoffice"; Components: office; Flags: ignoreversion recursesubdirs
Source: "{#Root}\deps\blender\*"; DestDir: "{app}\deps\blender"; Components: blender; Flags: ignoreversion recursesubdirs
Source: "{#Root}\target\licenses\*"; DestDir: "{app}\licenses"; Components: main; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Gregarioustypes settings"; Filename: "{app}\Gregarioustypes.exe"; Parameters: "--settings"; Comment: "Settings for the Space previews"
Name: "{autoprograms}\Gregarioustypes"; Filename: "{app}\Gregarioustypes.exe"; Comment: "Start the Space previews (tray icon)"

[Registry]
; The app manages "Start with Windows" itself; the task sets its first value
; and uninstalling removes it.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Gregarioustypes"; ValueData: """{app}\Gregarioustypes.exe"""; Tasks: autostart; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Gregarioustypes"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\Gregarioustypes.exe"; Description: "Start Gregarioustypes now"; Flags: nowait postinstall skipifsilent
Filename: "{app}\Gregarioustypes.exe"; Parameters: "--settings"; Description: "Open the settings"; Flags: nowait postinstall skipifsilent unchecked

[UninstallRun]
Filename: "{app}\Gregarioustypes.exe"; Parameters: "--quit"; RunOnceId: "QuitApp"; Flags: runhidden waituntilterminated

[UninstallDelete]
; Conversions, web view data and logs; settings (in Roaming) stay.
Type: filesandordirs; Name: "{localappdata}\Gregarioustypes"

[Code]
// Close a running copy before files are replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
  Exe: String;
begin
  Result := '';
  Exe := ExpandConstant('{app}\Gregarioustypes.exe');
  if FileExists(Exe) then
    Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

// The app's own setting follows the task (it keeps "Start with Windows" in sync).
procedure CurStepChanged(CurStep: TSetupStep);
var
  Path, Text: String;
begin
  if CurStep = ssPostInstall then
  begin
    Path := ExpandConstant('{userappdata}\Gregarioustypes\settings.toml');
    if not FileExists(Path) then
    begin
      if WizardIsTaskSelected('autostart') then Text := 'start_with_windows = true' else Text := 'start_with_windows = false';
      ForceDirectories(ExtractFileDir(Path));
      SaveStringToFile(Path, Text + #13#10, False);
    end;
  end;
end;
