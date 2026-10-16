; TLP-Desk - single setup.exe (Inno Setup 6.6+).
; Build with installer\build-setup.ps1, which passes /DMyAppVersion=<workspace version>
; and /DStageDir=<folder with the exes, README.txt and licenses\>.
;
; Upgrade-safe: AppId is fixed forever (never change it). Running TLP-Desk is closed through
; the Restart Manager. Uninstall keeps %ProgramData%\TLP-Desk (identity key = device ID) and
; %APPDATA%\TLP-Desk (settings) on purpose: a reinstall keeps the same 9-digit device ID.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef MyAppNumericVersion
  #define MyAppNumericVersion MyAppVersion
#endif
#ifndef StageDir
  #define StageDir "..\dist\setup-stage"
#endif
#define MyAppName "TLP-Desk"
#define MyAppExe "TLP-Desk.exe"
#define FwQuic "TLP-Desk (QUIC)"
#define FwDisc "TLP-Desk (discovery)"
#define FwMdns "TLP-Desk (mDNS)"

[Setup]
AppId={{2BD625D0-C78E-4CFB-BFEA-524A00171A2A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=TLP
AppCopyright=MIT OR Apache-2.0
VersionInfoVersion={#MyAppNumericVersion}
VersionInfoProductName={#MyAppName}
VersionInfoCompany=TLP
VersionInfoDescription={#MyAppName} setup
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
DisableWelcomePage=no
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename={#MyAppName}-{#MyAppVersion}-setup
SetupIconFile=..\crates\tlp-desk-app\assets\icon\TLP-Desk.ico
UninstallDisplayIcon={app}\{#MyAppExe}
UninstallDisplayName={#MyAppName}
WizardStyle=modern dynamic
WizardImageFile=assets\wizard-100.png,assets\wizard-125.png,assets\wizard-150.png,assets\wizard-175.png,assets\wizard-200.png,assets\wizard-225.png,assets\wizard-250.png
WizardSmallImageFile=assets\wizard-small-100.png,assets\wizard-small-125.png,assets\wizard-small-150.png,assets\wizard-small-175.png,assets\wizard-small-200.png,assets\wizard-small-225.png,assets\wizard-small-250.png
Compression=lzma2/ultra64
SolidCompression=yes
CloseApplications=yes
RestartApplications=no
; Language: preselected from the Windows UI language; the dialog lets the user change it.
LanguageDetectionMethod=uilanguage
ShowLanguageDialog=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "ar"; MessagesFile: "compiler:Languages\Arabic.isl"

[CustomMessages]
en.AppComment=Remote desktop for your local network
ar.AppComment=سطح مكتب بعيد لشبكتك المحلية
en.FirewallStatus=Adding Windows Firewall rules (private and domain networks only)...
ar.FirewallStatus=جارٍ إضافة قواعد جدار حماية ويندوز (للشبكات الخاصة وشبكات النطاق فقط)...
en.ServiceTask=Install as a Windows service (unattended access)
ar.ServiceTask=التثبيت كخدمة ويندوز (وصول غير مراقَب)
en.ServiceTaskGroup=Unattended access
ar.ServiceTaskGroup=الوصول غير المراقَب
en.ServiceStatus=Installing the TLP-Desk service...
ar.ServiceStatus=جارٍ تثبيت خدمة TLP-Desk...

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; Runs TLP-Desk as a SYSTEM service: reachable at the sign-in and lock screens and before login.
Name: "installservice"; Description: "{cm:ServiceTask}"; GroupDescription: "{cm:ServiceTaskGroup}"; Flags: unchecked

[Files]
Source: "{#StageDir}\TLP-Desk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\TLP-Desk-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#StageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Comment: "{cm:AppComment}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Comment: "{cm:AppComment}"; Tasks: desktopicon

[Run]
; Windows Firewall: inbound UDP for TLP-Desk.exe on Private + Domain profiles only (never Public).
; Delete first so upgrades/reinstalls never accumulate duplicates (a missing rule is harmless).
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwQuic}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwDisc}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwMdns}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
; Rules named by builds before the TLP-Desk rename (harmless no-op if already gone).
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""tlp-desk (QUIC)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""tlp-desk (discovery)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""tlp-desk (mDNS)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwQuic}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=47400 profile=private,domain enable=yes description=""TLP-Desk LAN remote desktop connections (QUIC/TLS 1.3)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwQuic}"" dir=in action=allow program=""{app}\TLP-Desk-cli.exe"" protocol=UDP localport=47400 profile=private,domain enable=yes description=""TLP-Desk CLI LAN remote desktop connections (QUIC/TLS 1.3)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwDisc}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=47401 profile=private,domain enable=yes description=""TLP-Desk LAN device discovery (broadcast)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwMdns}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=5353 profile=private,domain enable=yes description=""TLP-Desk LAN device discovery (mDNS)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
; Service: registered after the files are in place; an upgrade keeps an existing service.
Filename: "{app}\{#MyAppExe}"; Parameters: "install-service"; Flags: runhidden; StatusMsg: "{cm:ServiceStatus}"; Check: ShouldInstallService
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Stops and removes the service (does nothing when it was never installed).
Filename: "{app}\{#MyAppExe}"; Parameters: "uninstall-service"; Flags: runhidden; RunOnceId: "DelService"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwQuic}"""; Flags: runhidden; RunOnceId: "DelFwQuic"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwDisc}"""; Flags: runhidden; RunOnceId: "DelFwDisc"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwMdns}"""; Flags: runhidden; RunOnceId: "DelFwMdns"

; Intentionally no [UninstallDelete] for {commonappdata}\TLP-Desk: keeps the device identity.

[Code]
var
  ServiceWasInstalled: Boolean;

{ The service key exists once install-service ran. }
function ServiceKeyExists: Boolean;
begin
  Result := RegKeyExists(HKLM, 'SYSTEM\CurrentControlSet\Services\TLP-Desk');
end;

{ The ticked task, or an existing service that an upgrade must keep. }
function ShouldInstallService: Boolean;
begin
  Result := WizardIsTaskSelected('installservice') or ServiceWasInstalled;
end;

{ An upgrade replaces TLP-Desk.exe, which the running service holds open: stop and remove it
  first; the [Run] entry above registers it again. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  Exe: String;
begin
  Result := '';
  ServiceWasInstalled := ServiceKeyExists;
  Exe := ExpandConstant('{app}\TLP-Desk.exe');
  if ServiceWasInstalled and FileExists(Exe) then
    Exec(Exe, 'uninstall-service', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;
