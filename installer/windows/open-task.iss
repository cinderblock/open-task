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
;   X64Com       path to the x86_64-pc-windows-msvc open-task.com (console launcher)
;   Arm64Com     path to the aarch64-pc-windows-msvc open-task.com
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
;
; The "replacetaskmgr" task makes Windows start open-task in Task Manager's place, as
; the app's Settings page can (crates/ot-shell-win/src/task_manager.rs): a Debugger
; value under Task Manager's Image File Execution Options key. It is offered only for
; an all-users install, and its checkbox shows what Windows does now rather than what
; was chosen last time, since the app may have changed it since. A silent install,
; which every update is, leaves it alone unless /TASKS or /MERGETASKS names it. The
; uninstaller removes the value whenever it names this install, whoever set it: a
; value naming a missing file would leave Ctrl+Shift+Esc doing nothing.

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef X64Exe
  #error Pass /DX64Exe=path\to\x64\open-task.exe
#endif
#ifndef Arm64Exe
  #error Pass /DArm64Exe=path\to\arm64\open-task.exe
#endif
#ifndef X64Com
  #error Pass /DX64Com=path\to\x64\open-task.com
#endif
#ifndef Arm64Com
  #error Pass /DArm64Com=path\to\arm64\open-task.com
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
Name: "replacetaskmgr"; Description: "Use open-task instead of Task Manager (Ctrl+Shift+Esc, the taskbar, Ctrl+Alt+Del)"; GroupDescription: "Task Manager:"; Flags: unchecked; Check: IsAdminInstallMode

[Files]
Source: "{#X64Exe}"; DestDir: "{app}"; DestName: "open-task.exe"; Flags: ignoreversion; Check: not IsArm64
Source: "{#Arm64Exe}"; DestDir: "{app}"; DestName: "open-task.exe"; Flags: ignoreversion; Check: IsArm64
; The console launcher: what `open-task` runs from a terminal (.COM comes before .EXE
; in PATHEXT), so the shell waits for --version, --headless and --check-update.
Source: "{#X64Com}"; DestDir: "{app}"; DestName: "open-task.com"; Flags: ignoreversion; Check: not IsArm64
Source: "{#Arm64Com}"; DestDir: "{app}"; DestName: "open-task.com"; Flags: ignoreversion; Check: IsArm64
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
  TaskManagerKey = 'SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe';

var
  TasksPageSeen: Boolean;

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

{ The program a Debugger value names: what is inside the quotes, or the whole value
  when it is not quoted. open-task always writes its path quoted. }
function ProgramOf(Value: String): String;
var
  P: Integer;
begin
  Result := Trim(Value);
  if Copy(Result, 1, 1) = '"' then
  begin
    Delete(Result, 1, 1);
    P := Pos('"', Result);
    if P > 0 then
      Result := Copy(Result, 1, P - 1);
  end;
end;

{ Whether Windows starts this install's open-task in Task Manager's place. }
function TaskManagerIsOurs: Boolean;
var
  Value: String;
begin
  Result := RegQueryStringValue(HKLM64, TaskManagerKey, 'Debugger', Value) and
    (CompareText(ProgramOf(Value), ExpandConstant('{app}\open-task.exe')) = 0);
end;

procedure ReplaceTaskManager;
begin
  if not RegWriteStringValue(HKLM64, TaskManagerKey, 'Debugger',
                             '"' + ExpandConstant('{app}\open-task.exe') + '"') then
    Log('Could not replace Task Manager');
end;

{ Only when it is this install: another program's replacement is not ours to undo.
  The key goes too if nothing else is in it. }
procedure RestoreTaskManager;
begin
  if TaskManagerIsOurs then
  begin
    RegDeleteValue(HKLM64, TaskManagerKey, 'Debugger');
    RegDeleteKeyIfEmpty(HKLM64, TaskManagerKey);
  end;
end;

{ Whether /TASKS or /MERGETASKS names the task, for it or against it. }
function TaskManagerNamed: Boolean;
begin
  Result := Pos('replacetaskmgr', Lowercase(ExpandConstant('{param:TASKS|}') + ',' +
                                            ExpandConstant('{param:MERGETASKS|}'))) > 0;
end;

{ Whether this run decides the task: an interactive install does, where the checkbox
  shows the current state; a silent one (every update) only when the command line
  names the task, so an update never undoes what the user set in the app since. }
function TaskManagerChoiceGiven: Boolean;
begin
  Result := (not WizardSilent) or TaskManagerNamed;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  { The first time the tasks page shows, tick "replacetaskmgr" if and only if Windows
    starts this install in Task Manager's place now. Only once, so going back and
    forth keeps the user's own click. Not when the command line chose, and not in a
    silent install, which steps through the pages too without showing them. }
  if (CurPageID = wpSelectTasks) and not TasksPageSeen then
  begin
    TasksPageSeen := True;
    if IsAdminInstallMode and not WizardSilent and not TaskManagerNamed then
    begin
      if TaskManagerIsOurs then
        WizardSelectTasks('replacetaskmgr')
      else
        WizardSelectTasks('!replacetaskmgr');
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('addtopath') then
      AddToPath(ExpandConstant('{app}'));
    if IsAdminInstallMode and TaskManagerChoiceGiven then
    begin
      if WizardIsTaskSelected('replacetaskmgr') then
        ReplaceTaskManager
      else
        RestoreTaskManager;
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  { Before the files go, whoever set it: a Debugger value that names a missing file
    leaves Ctrl+Shift+Esc doing nothing, for every user. }
  if (CurUninstallStep = usUninstall) and TaskManagerIsOurs then
  begin
    if IsAdminInstallMode then
      RestoreTaskManager
    else
    begin
      { A per-user uninstall cannot write HKLM; the app can, as administrator. }
      if not ShellExec('runas', ExpandConstant('{app}\open-task.exe'), '--restore-task-manager',
                       '', SW_HIDE, ewWaitUntilTerminated, Code) then
        Log('Could not start open-task to restore Task Manager: ' + SysErrorMessage(Code));
      if TaskManagerIsOurs and not UninstallSilent then
        MsgBox('Windows still starts open-task in Task Manager''s place, and it will be gone, ' +
               'so Ctrl+Shift+Esc will do nothing. To bring Task Manager back, delete the ' +
               '"Debugger" value under HKEY_LOCAL_MACHINE\' + TaskManagerKey +
               ' as administrator.', mbError, MB_OK);
    end;
  end;
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;
