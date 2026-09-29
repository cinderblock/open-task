; open-task Windows installer.
;
; Compiled by scripts/build-installer.ps1, locally and in .github/workflows/release.yml.
; One installer carries both the x64 and the ARM64 binary and installs the one that
; matches the machine. It installs for all users into Program Files and needs
; elevation (a UAC prompt), also for every update. That is deliberate: a task manager
; gets run elevated, and a binary in a user-writable folder that is launched elevated
; is a privilege-escalation path for anything running as that user. A per-user
; install into the user's profile is available only by asking for it explicitly with
; /CURRENTUSER on the command line; the wizard does not offer it.
;
; Required defines (the build script passes them):
;   AppVersion   version string, e.g. 0.2.1 or 0.3.0-pre.1
;   X64Exe       path to the x86_64-pc-windows-msvc open-task.exe
;   Arm64Exe     path to the aarch64-pc-windows-msvc open-task.exe
; Optional:
;   VersionInfoVersion   numeric x.y.z for the file version resource (default AppVersion)
;   OutputDir            where the setup .exe goes (default: the script's directory)
;
; The app updates itself by running this installer (crates/ot-update):
;   /SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /NORESTARTAPPLICATIONS
;   /RELAUNCH=1 and /ALLUSERS or /CURRENTUSER to match the install.
; Setup closes the running app through Restart Manager, which the app answers
; (WM_ENDSESSION), installs, and with /RELAUNCH=1 starts the new version. Keep AppId
; in step with crates/ot-update/src/imp/windows.rs, which finds the install by it.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef X64Exe
  #error Pass /DX64Exe=path\to\x64\open-task.exe
#endif
#ifndef Arm64Exe
  #error Pass /DArm64Exe=path\to\arm64\open-task.exe
#endif
#ifndef VersionInfoVersion
  #define VersionInfoVersion AppVersion
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif
#if Ver < EncodeVer(6,3,0)
  #error Inno Setup 6.3 or newer is required (x64compatible, IsArm64)
#endif

[Setup]
; Never change AppId: it is how Windows and Inno recognise an existing install to
; upgrade in place.
AppId={{9B1C3F2E-6D7A-4A0E-9B4B-2C8F1E5D7A31}
AppName=open-task
AppVersion={#AppVersion}
AppVerName=open-task {#AppVersion}
AppPublisher=Cameron Tacklind
AppPublisherURL=https://github.com/cinderblock/open-task
AppSupportURL=https://github.com/cinderblock/open-task/issues
AppUpdatesURL=https://github.com/cinderblock/open-task/releases
VersionInfoVersion={#VersionInfoVersion}
VersionInfoDescription=open-task installer
DefaultDirName={autopf}\open-task
DefaultGroupName=open-task
DisableProgramGroupPage=yes
; Per machine, elevated. `commandline` lets /CURRENTUSER opt into a per-user install
; without ever showing a dialog that suggests it.
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\..\LICENSE
OutputDir={#OutputDir}
OutputBaseFilename=open-task-v{#AppVersion}-windows-setup
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
; The logo (assets/logo, made by scripts/render-logo.ps1): Setup's and the
; uninstaller's icon, and the wizard's corner image in each size Setup uses at
; 100% to 250% scaling. PNG wizard images need Inno Setup 6.5.2 (GitHub's Windows
; runners have 6.7.1); an older compiler keeps its built-in image rather than failing.
SetupIconFile=..\..\assets\logo\open-task.ico
#if Ver >= EncodeVer(6,5,2)
WizardSmallImageFile=..\..\assets\logo\installer\wizard-58.png,..\..\assets\logo\installer\wizard-77.png,..\..\assets\logo\installer\wizard-97.png,..\..\assets\logo\installer\wizard-116.png,..\..\assets\logo\installer\wizard-124.png,..\..\assets\logo\installer\wizard-143.png,..\..\assets\logo\installer\wizard-159.png
#endif
UninstallDisplayName=open-task
UninstallDisplayIcon={app}\open-task.exe
; Setup and the uninstaller broadcast the environment change when PATH is edited.
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add open-task to PATH (for ""open-task --headless"" in a terminal)"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "{#X64Exe}"; DestDir: "{app}"; DestName: "open-task.exe"; Flags: ignoreversion; Check: not IsArm64
Source: "{#Arm64Exe}"; DestDir: "{app}"; DestName: "open-task.exe"; Flags: ignoreversion; Check: IsArm64
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\open-task"; Filename: "{app}\open-task.exe"
Name: "{autodesktop}\open-task"; Filename: "{app}\open-task.exe"; Tasks: desktopicon

[Run]
; runasoriginaluser: the app launched from the wizard's last page must run as the
; user, not with Setup's elevation. Inno 6 defaults postinstall entries to this;
; stated here so nobody has to remember that.
Filename: "{app}\open-task.exe"; Description: "{cm:LaunchProgram,open-task}"; Flags: nowait postinstall skipifsilent runasoriginaluser
; After an update the app asked for (/RELAUNCH=1), start the new version. Silent, so
; not a postinstall entry; runasoriginaluser starts it as the user who ran the app,
; not elevated, because the app starts Setup unelevated and Setup elevates itself.
; (Started from an elevated app, Setup is elevated from the start, and so is this.)
Filename: "{app}\open-task.exe"; Flags: nowait runasoriginaluser; Check: RelaunchRequested

[Code]
const
  MachineEnvKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  UserEnvKey = 'Environment';

{ The app's updater passes /RELAUNCH=1: start the new version when done. }
function RelaunchRequested: Boolean;
begin
  Result := ExpandConstant('{param:RELAUNCH|0}') = '1';
end;

{ PATH lives in HKLM for an all-users install and HKCU for a per-user one. }
function EnvRootKey: Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

function EnvSubkey: String;
begin
  if IsAdminInstallMode then
    Result := MachineEnvKey
  else
    Result := UserEnvKey;
end;

{ Path split at semicolons and rebuilt without any entry equal to Dir, so adding is
  idempotent across upgrades and removing takes out exactly our entry. }
function PathWithout(Path, Dir: String): String;
var
  Entry: String;
  P: Integer;
begin
  Result := '';
  Path := Path + ';';
  while Length(Path) > 0 do
  begin
    P := Pos(';', Path);
    Entry := Copy(Path, 1, P - 1);
    Delete(Path, 1, P);
    if (Entry <> '') and
       (CompareText(RemoveBackslashUnlessRoot(Entry), RemoveBackslashUnlessRoot(Dir)) <> 0) then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Entry;
    end;
  end;
end;

procedure AddToPath(Dir: String);
var
  Path: String;
begin
  if not RegQueryStringValue(EnvRootKey, EnvSubkey, 'Path', Path) then
    Path := '';
  Path := PathWithout(Path, Dir);
  if Path <> '' then
    Path := Path + ';';
  RegWriteExpandStringValue(EnvRootKey, EnvSubkey, 'Path', Path + Dir);
end;

procedure RemoveFromPath(Dir: String);
var
  Path: String;
begin
  if RegQueryStringValue(EnvRootKey, EnvSubkey, 'Path', Path) then
    RegWriteExpandStringValue(EnvRootKey, EnvSubkey, 'Path', PathWithout(Path, Dir));
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
