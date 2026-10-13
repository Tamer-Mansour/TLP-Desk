; tlp-desk - single setup.exe (Inno Setup 6.6+).
; Build with installer\build-setup.ps1, which passes /DMyAppVersion=<workspace version>
; and /DStageDir=<folder with the exes, README.txt and licenses\>.
;
; Upgrade-safe: AppId is fixed forever (never change it). Running tlp-desk is closed through
; the Restart Manager. Uninstall keeps %ProgramData%\tlp-desk (identity key = device ID) and
; %APPDATA%\tlp-desk (settings) on purpose: a reinstall keeps the same 9-digit device ID.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\dist\setup-stage"
#endif
#define MyAppName "tlp-desk"
#define MyAppExe "tlp-desk.exe"
#define FwQuic "tlp-desk (QUIC)"
#define FwDisc "tlp-desk (discovery)"
#define FwMdns "tlp-desk (mDNS)"

[Setup]
AppId={{2BD625D0-C78E-4CFB-BFEA-524A00171A2A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=TLP
AppCopyright=MIT OR Apache-2.0
VersionInfoVersion={#MyAppVersion}
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
SetupIconFile=..\crates\tlp-desk-app\assets\icon\tlp-desk.ico
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

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#StageDir}\tlp-desk.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\tlp-desk-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#StageDir}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#StageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Comment: "{cm:AppComment}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExe}"; Comment: "{cm:AppComment}"; Tasks: desktopicon

[Run]
; Windows Firewall: inbound UDP for tlp-desk.exe on Private + Domain profiles only (never Public).
; Delete first so upgrades/reinstalls never accumulate duplicates (a missing rule is harmless).
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwQuic}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwDisc}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwMdns}"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwQuic}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=47400 profile=private,domain enable=yes description=""tlp-desk LAN remote desktop connections (QUIC/TLS 1.3)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwQuic}"" dir=in action=allow program=""{app}\tlp-desk-cli.exe"" protocol=UDP localport=47400 profile=private,domain enable=yes description=""tlp-desk CLI LAN remote desktop connections (QUIC/TLS 1.3)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwDisc}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=47401 profile=private,domain enable=yes description=""tlp-desk LAN device discovery (broadcast)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""{#FwMdns}"" dir=in action=allow program=""{app}\{#MyAppExe}"" protocol=UDP localport=5353 profile=private,domain enable=yes description=""tlp-desk LAN device discovery (mDNS)"""; Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "{app}\{#MyAppExe}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwQuic}"""; Flags: runhidden; RunOnceId: "DelFwQuic"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwDisc}"""; Flags: runhidden; RunOnceId: "DelFwDisc"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""{#FwMdns}"""; Flags: runhidden; RunOnceId: "DelFwMdns"

; Intentionally no [UninstallDelete] for {commonappdata}\tlp-desk: keeps the device identity.
