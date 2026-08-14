; TGZTerminal's Windows installer.
;
; Derived from upstream WezTerm's ci/windows-installer.iss (which remains in this
; tree, unmodified, for upstream's own release scripts). This copy exists because
; the fork's requirements diverge structurally: a per-user install by default, an
; AppId of its own, an opt-in PATH task, and a migration away from the shared
; upstream AppId. Keeping them as two files means neither needs untangling when
; upstream changes theirs.
;
; vim:ts=2:sw=2:et:

; Branding. Each of these may be supplied on the iscc command line
; (//DMyAppName=... etc); the #ifndef guards mean the values below are
; defaults only. A bare `#define` would silently overwrite a command-line
; define, which is why they are not used here.
#ifndef MyAppName
  #define MyAppName "TGZTerminal"
#endif
#ifndef MyAppPublisher
  #define MyAppPublisher "Tim Grossinger"
#endif
#ifndef MyAppURL
  #define MyAppURL "https://github.com/timjensgrossinger/tgzterminal"
#endif
#define MyAppExeName "wezterm-gui.exe"
#define MyCliExeName "tgzterminal.exe"

; Marker file that switches the app into portable mode, where config next to the
; executable outranks the user's own (see config::PORTABLE_MARKER). The portable
; zip ships one; an installed tree must never have one, hence [InstallDelete].
#define PortableMarker ".portable"

; The GUI calls SetCurrentProcessExplicitAppUserModelID with this exact string
; (wezterm-gui/src/main.rs) and the toast backend registers under it
; (wezterm-toast-notification/src/windows.rs). Windows only delivers toasts for
; an AUMID that matches a Start Menu shortcut, so this must not be rebranded
; independently of that code.
#define MyAppUserModelID "org.wezfurlong.wezterm"

; Upstream WezTerm's AppId, which this installer used to share. Referenced only
; by the migration code below, which offers to remove such an install.
#define LegacySharedAppId "{BCF6F0DA-5B9A-408D-8562-F680AE6E1EAF}"

[Setup]
; TGZTerminal's own AppId. NEVER CHANGE THIS: Windows identifies an installed
; application by it, so a new value orphans every existing install (a second
; Apps & Features entry, a second copy on disk, duplicate context-menu entries).
; It differs from upstream's deliberately -- sharing theirs made Windows treat
; TGZTerminal and WezTerm as one application.
AppId={{8638970E-1E70-438A-AF45-7523FCDB720F}
; x64compatible covers x64 plus arm64 that can emulate x64, which is exactly
; what upstream's hand-rolled GetMachineTypeAttributes probe computed. Requires
; Inno Setup 6.3+, pinned in the release workflow. Only x64 binaries are built
; (the vendored ANGLE/conpty/OpenConsole/mesa/fzf payload is x64-only), so arm64
; runs under emulation.
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
; Per-user by default: no UAC prompt, and the updater can replace files without
; elevation. `dialog` still lets someone who launches the setup elevated pick an
; all-users install, which is the escape hatch for managed machines where policy
; only permits execution from Program Files.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; {autopf} resolves to {localappdata}\Programs in per-user mode and to
; Program Files in admin mode, so one line covers both.
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
OutputDir=..
#ifndef MyOutputBaseFilename
  #define MyOutputBaseFilename MyAppName + "-Setup"
#endif
OutputBaseFilename={#MyOutputBaseFilename}
; A numeric file version for the setup executable itself; AppVersion carries the
; human tag (tgz-vYYYY.MM.PATCH), which is not a valid VERSIONINFO value.
#ifdef MyVersionInfoVersion
VersionInfoVersion={#MyVersionInfoVersion}
#endif
SetupIconFile=..\assets\windows\terminal.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma
SolidCompression=yes
WizardStyle=modern
; Build 1809 is required for pty support
MinVersion=10.0.17763
ChangesEnvironment=true
; Two copies of setup racing each other over the same install directory ends
; badly; make the second one wait.
SetupMutex={#MyAppName}Setup
; Upgrading over a running install otherwise fails on locked binaries. Restart
; Manager asks the running GUI and mux server to close, then brings them back
; afterwards.
CloseApplications=yes
RestartApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; Opt-in, and unchecked: modifying PATH is a change to the user's environment,
; not part of installing an application. Without it the Start Menu entry and the
; Explorer context menu still work; only the `tgzterminal` CLI needs it.
Name: "addtopath"; Description: "Add {#MyAppName} to my PATH (so ""tgzterminal"" works in any shell)"; GroupDescription: "Command line:"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyCliExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\wezterm-gui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\wezterm-mux-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\mesa\opengl32.dll"; DestDir: "{app}\mesa"; Flags: ignoreversion
Source: "..\target\release\libEGL.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\libGLESv2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\conpty.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\OpenConsole.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\strip-ansi-escapes.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\fzf.exe"; DestDir: "{app}"; Flags: ignoreversion
; Deliberately no wildcards and no skipifsourcedoesntexist: a missing payload
; file must fail the build loudly rather than ship a broken install.

[InstallDelete]
; Defensive: an installed tree must never inherit a portable marker from a zip
; someone extracted over the top of it.
Type: files; Name: "{app}\{#PortableMarker}"

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "{#MyAppUserModelID}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; AppUserModelID: "{#MyAppUserModelID}"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Registry]
; HKA resolves to HKCU in per-user mode and HKLM in admin mode, and per-user
; shell verbs are fully supported, so these need no change for either mode.
Root: HKA; Subkey: "Software\Classes\Drive\shell\Open {#MyAppName} here"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Drive\shell\Open {#MyAppName} here"; ValueName: "icon"; ValueType: string; ValueData: "{app}\{#MyAppExeName}"; Flags: uninsdeletekey;
Root: HKA; Subkey: "Software\Classes\Drive\shell\Open {#MyAppName} here\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" start --no-auto-connect --cwd ""%V\"""; Flags: uninsdeletekey;
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open {#MyAppName} here"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open {#MyAppName} here"; ValueName: "icon"; ValueType: string; ValueData: "{app}\{#MyAppExeName}"; Flags: uninsdeletekey;
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Open {#MyAppName} here\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" start --no-auto-connect --cwd ""%V"; Flags: uninsdeletekey;
Root: HKA; Subkey: "Software\Classes\Directory\shell\Open {#MyAppName} here"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\shell\Open {#MyAppName} here"; ValueName: "icon"; ValueType: string; ValueData: "{app}\{#MyAppExeName}"; Flags: uninsdeletekey;
Root: HKA; Subkey: "Software\Classes\Directory\shell\Open {#MyAppName} here\command"; ValueType: string; ValueData: """{app}\{#MyAppExeName}"" start --no-auto-connect --cwd ""%V\\"""; Flags: uninsdeletekey;

[Code]
{ PATH lives in a different hive depending on the install mode. Per-user writes
  HKCU\Environment; an all-users install writes the machine environment. }
const
  UserEnvironmentKey = 'Environment';
  MachineEnvironmentKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  LegacyUninstallSubkey =
    'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#LegacySharedAppId}_is1';

function EnvRootKey(): Integer;
begin
  if IsAdminInstallMode() then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

function EnvKeyPath(): String;
begin
  if IsAdminInstallMode() then
    Result := MachineEnvironmentKey
  else
    Result := UserEnvironmentKey;
end;

{ https://stackoverflow.com/a/46609047/149111 }
procedure EnvAddPath(instlPath: string);
var
  Paths: string;
begin
  { Retrieve current path (use empty string if entry not exists) }
  if not RegQueryStringValue(EnvRootKey(), EnvKeyPath(), 'Path', Paths) then
    Paths := '';

  if Paths = '' then
    Paths := instlPath + ';'
  else
  begin
    { Skip if string already found in path }
    if Pos(';' + Uppercase(instlPath) + ';',  ';' + Uppercase(Paths) + ';') > 0 then exit;
    if Pos(';' + Uppercase(instlPath) + '\;', ';' + Uppercase(Paths) + ';') > 0 then exit;

    { Append App Install Path to the end of the path variable }
    if Paths[length(Paths)] <> ';' then
      Paths := Paths + ';';

    Paths := Paths + instlPath + ';';
  end;

  { Written as REG_EXPAND_SZ: a user's Path commonly contains %USERPROFILE%-style
    references, and rewriting the value as a plain string would freeze them. }
  if RegWriteExpandStringValue(EnvRootKey(), EnvKeyPath(), 'Path', Paths)
  then Log(Format('The [%s] added to PATH: [%s]', [instlPath, Paths]))
  else Log(Format('Error while adding the [%s] to PATH: [%s]', [instlPath, Paths]));
end;

procedure EnvRemovePath(instlPath: string);
var
  Paths: string;
  P, Offset, DelimLen: Integer;
begin
  { Skip if registry entry not exists }
  if not RegQueryStringValue(EnvRootKey(), EnvKeyPath(), 'Path', Paths) then
    exit;

  { Skip if string not found in path }
  DelimLen := 1;     { Length(';') }
  P := Pos(';' + Uppercase(instlPath) + ';', ';' + Uppercase(Paths) + ';');
  if P = 0 then
  begin
    { perhaps instlPath lives in Paths, but terminated by '\;' }
    DelimLen := 2; { Length('\;') }
    P := Pos(';' + Uppercase(instlPath) + '\;', ';' + Uppercase(Paths) + ';');
    if P = 0 then exit;
  end;

  { Decide where to start string subset in Delete() operation. }
  if P = 1 then
    Offset := 0
  else
    Offset := 1;
  { Update path variable }
  Delete(Paths, P - Offset, Length(instlPath) + DelimLen);

  { Overwrite path environment variable }
  if RegWriteExpandStringValue(EnvRootKey(), EnvKeyPath(), 'Path', Paths)
  then Log(Format('The [%s] removed from PATH: [%s]', [instlPath, Paths]))
  else Log(Format('Error while removing the [%s] from PATH: [%s]', [instlPath, Paths]));
end;

{ Earlier builds registered the shell verbs under the upstream "Open WezTerm
  here" name. The [Registry] entries above now use {#MyAppName}, so upgrading
  such an install would leave the old keys behind as orphans pointing at the
  same executable -- two identical context menu entries. Remove them.
  HKA resolves to HKLM in an administrative install and HKCU otherwise, so
  both hives are cleaned; a missing key is not an error, and the HKLM deletes
  simply fail harmlessly in a per-user install. }
procedure RemoveLegacyShellVerbs();
var
  Keys: array[0..2] of String;
  I: Integer;
begin
  if CompareText('{#MyAppName}', 'WezTerm') = 0 then
    exit;

  Keys[0] := 'Software\Classes\Drive\shell\Open WezTerm here';
  Keys[1] := 'Software\Classes\Directory\Background\shell\Open WezTerm here';
  Keys[2] := 'Software\Classes\Directory\shell\Open WezTerm here';

  for I := 0 to 2 do
  begin
    RegDeleteKeyIncludingSubkeys(HKEY_LOCAL_MACHINE, Keys[I]);
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, Keys[I]);
  end;
end;

{ ---------------------------------------------------------------------------
  Migration off the shared upstream AppId.

  Builds up to tgz-v2026.08.4 installed per-machine under upstream WezTerm's
  AppId, so Windows cannot tell the two apart by id alone -- and installing ours
  over a real WezTerm would have replaced WezTerm's uninstall entry. Everything
  below is therefore built around one rule: never offer to remove something we
  cannot positively identify as our own.
  --------------------------------------------------------------------------- }

{ Find a legacy install and return what we know about it. Probes both 64- and
  32-bit views of HKLM (a 32-bit-mode Inno install lands under Wow6432Node) and
  then HKCU. }
function FindLegacyInstall(var RootKey: Integer; var DisplayName, InstallLocation,
  UninstallCmd: String): Boolean;
var
  Roots: array[0..2] of Integer;
  I: Integer;
begin
  Result := False;
  Roots[0] := HKLM64;
  Roots[1] := HKLM32;
  Roots[2] := HKEY_CURRENT_USER;

  for I := 0 to 2 do
  begin
    if RegKeyExists(Roots[I], LegacyUninstallSubkey) then
    begin
      RootKey := Roots[I];
      if not RegQueryStringValue(Roots[I], LegacyUninstallSubkey, 'DisplayName', DisplayName) then
        DisplayName := '';
      if not RegQueryStringValue(Roots[I], LegacyUninstallSubkey, 'InstallLocation', InstallLocation) then
        InstallLocation := '';
      if not RegQueryStringValue(Roots[I], LegacyUninstallSubkey, 'QuietUninstallString', UninstallCmd) then
      begin
        if not RegQueryStringValue(Roots[I], LegacyUninstallSubkey, 'UninstallString', UninstallCmd) then
          UninstallCmd := '';
      end;
      Result := True;
      exit;
    end;
  end;
end;

{ Is that legacy install ours rather than a genuine WezTerm?

  The strong signal is the CLI binary name: this fork renames it to
  tgzterminal.exe (wezterm/Cargo.toml [[bin]] name), and upstream ships
  wezterm.exe, never this. The DisplayName check covers an install whose
  directory has already been removed by hand. Anything else is treated as
  somebody else's application and left completely alone. }
function LegacyInstallIsOurs(const DisplayName, InstallLocation: String): Boolean;
begin
  Result := False;
  if (InstallLocation <> '') and FileExists(AddBackslash(InstallLocation) + '{#MyCliExeName}') then
    Result := True
  else if CompareText(DisplayName, '{#MyAppName}') = 0 then
    Result := True;
end;

{ Copy a config left behind in the legacy install directory.

  That install honoured a wezterm.lua next to the executable unconditionally;
  this build only does so in portable mode, so such a file would silently stop
  being read. Copy (never move) it into the user profile when they have no config
  of their own, and only mention the directories -- moving a tree without asking
  is too much. }
procedure RescueLegacyConfig(const InstallLocation: String);
var
  LegacyConfig, UserConfig, XdgConfig, Extras: String;
begin
  if InstallLocation = '' then
    exit;
  LegacyConfig := AddBackslash(InstallLocation) + 'wezterm.lua';
  if not FileExists(LegacyConfig) then
    exit;

  UserConfig := ExpandConstant('{userprofile}\.wezterm.lua');
  XdgConfig := ExpandConstant('{userprofile}\.config\wezterm\wezterm.lua');
  if FileExists(UserConfig) or FileExists(XdgConfig) then
    exit;

  if SuppressibleMsgBox(
       'A configuration file was found in the old install folder:' + #13#10#13#10 +
       LegacyConfig + #13#10#13#10 +
       'This version only reads a configuration next to the program when it is ' +
       'running in portable mode, so that file would be ignored. Copy it to ' +
       UserConfig + ' now? The original is left untouched.',
       mbConfirmation, MB_YESNO, IDNO) = IDYES then
  begin
    if FileCopy(LegacyConfig, UserConfig, True) then
      Log('Copied legacy config to ' + UserConfig)
    else
      Log('Failed to copy legacy config from ' + LegacyConfig);
  end;

  Extras := '';
  if DirExists(AddBackslash(InstallLocation) + 'colors') then
    Extras := Extras + #13#10 + AddBackslash(InstallLocation) + 'colors';
  if DirExists(AddBackslash(InstallLocation) + 'wezterm_modules') then
    Extras := Extras + #13#10 + AddBackslash(InstallLocation) + 'wezterm_modules';
  if Extras <> '' then
    SuppressibleMsgBox(
      'These folders in the old install location will also stop being read:' + #13#10 +
      Extras + #13#10#13#10 +
      'Copy anything you want to keep before removing the old install.',
      mbInformation, MB_OK, IDOK);
end;

{ Run the legacy uninstaller and wait for it to actually finish.

  Inno's uninstaller copies itself into %TEMP% and the process we launch returns
  immediately, so ewWaitUntilTerminated is not enough on its own -- poll for the
  uninstall key to disappear. }
function RunLegacyUninstaller(RootKey: Integer; const UninstallCmd: String): Boolean;
var
  Cmd: String;
  ResultCode, Waited: Integer;
begin
  Cmd := RemoveQuotes(UninstallCmd);
  if Cmd = '' then
  begin
    Result := False;
    exit;
  end;

  if not Exec(Cmd, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '', SW_SHOW,
              ewWaitUntilTerminated, ResultCode) then
  begin
    { Most likely the user declined the elevation prompt. }
    Log('Could not start the legacy uninstaller');
    Result := False;
    exit;
  end;

  Waited := 0;
  while RegKeyExists(RootKey, LegacyUninstallSubkey) and (Waited < 60000) do
  begin
    Sleep(500);
    Waited := Waited + 500;
  end;
  Result := not RegKeyExists(RootKey, LegacyUninstallSubkey);
  if not Result then
    Log('Legacy uninstall did not complete within the timeout');
end;

procedure MigrateLegacyInstall();
var
  RootKey: Integer;
  DisplayName, InstallLocation, UninstallCmd: String;
begin
  if not FindLegacyInstall(RootKey, DisplayName, InstallLocation, UninstallCmd) then
    exit;

  if not LegacyInstallIsOurs(DisplayName, InstallLocation) then
  begin
    { Almost certainly a real WezTerm install, which is none of our business. }
    Log('Found an install under the shared AppId that is not ours ("' +
        DisplayName + '" at "' + InstallLocation + '"); leaving it alone');
    exit;
  end;

  { IDNO default, so a /SILENT install never removes anything. }
  if SuppressibleMsgBox(
       'An older ' + '{#MyAppName}' + ' is installed for all users at:' + #13#10#13#10 +
       InstallLocation + #13#10#13#10 +
       'This version installs into your user profile instead. Remove the old ' +
       'all-users copy now? Windows will ask for administrator approval.' + #13#10#13#10 +
       'Choosing No leaves two ' + '{#MyAppName}' + ' entries in Apps & Features, ' +
       'two copies on disk, and two "Open ' + '{#MyAppName}' + ' here" context-menu entries.',
       mbConfirmation, MB_YESNO, IDNO) <> IDYES then
  begin
    Log('User declined removal of the legacy all-users install');
    exit;
  end;

  RescueLegacyConfig(InstallLocation);

  if not RunLegacyUninstaller(RootKey, UninstallCmd) then
    SuppressibleMsgBox(
      'The old all-users copy was left in place. You can remove ' +
      '{#MyAppName}' + ' from Apps & Features later; this installation will ' +
      'continue either way.',
      mbInformation, MB_OK, IDOK);
end;

<event('InitializeSetup')>
function InitializeSetupMigrateLegacy(): Boolean;
begin
  MigrateLegacyInstall();
  Result := True;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('addtopath') then
      EnvAddPath(ExpandConstant('{app}'));
    RemoveLegacyShellVerbs();
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { Unconditional: EnvRemovePath already no-ops when the entry is absent, so this
    also cleans up after someone who ticked the task and later changed their mind
    about keeping it. }
  if CurUninstallStep = usPostUninstall then
    EnvRemovePath(ExpandConstant('{app}'));
end;
