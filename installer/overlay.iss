; Installer for the overlay program (the gaming computer). Build with installer\build.ps1.

#ifndef AppVersion
  #define AppVersion "0.2.0"
#endif
#define Exe "ts_lan_overlay.exe"

[Setup]
AppId={{2BC81EE0-A2F9-4A08-9080-E39A7449AA96}
AppName=TS LAN Overlay
AppVersion={#AppVersion}
AppPublisher=TakeChance
DefaultDirName={autopf}\TS LAN Overlay
DisableProgramGroupPage=yes
UninstallDisplayName=TS LAN Overlay
UninstallDisplayIcon={app}\{#Exe}
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=ts_lan_overlay_setup_{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Messages]
WelcomeLabel2=Этот установщик ставится на игровой компьютер. На компьютер с TeamSpeak 3 ставится плагин (ts_lan_overlay_plugin_setup).
FinishedLabel=Программа установлена. Её значок появится в трее рядом с часами; через него открываются настройки.

[Tasks]
Name: "autostart"; Description: "Запускать вместе с Windows"

[Files]
Source: "..\target\release\{#Exe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\Инструкция по установке.txt"; DestDir: "{app}"; DestName: "Инструкция.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\TS LAN Overlay"; Filename: "{app}\{#Exe}"
Name: "{commonstartup}\TS LAN Overlay"; Filename: "{app}\{#Exe}"; Tasks: autostart

[Run]
; Packets from the TS computer arrive unsolicited, so Windows Firewall must let them in on any network type.
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""TS LAN Overlay"""; Flags: runhidden
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall add rule name=""TS LAN Overlay"" dir=in action=allow program=""{app}\{#Exe}"" protocol=UDP localport=39391 profile=any"; Flags: runhidden
Filename: "{app}\{#Exe}"; Description: "Запустить TS LAN Overlay"; Flags: postinstall nowait skipifsilent runasoriginaluser
Filename: "{app}\Инструкция.txt"; Description: "Открыть инструкцию"; Flags: postinstall shellexec skipifsilent unchecked

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im {#Exe}"; Flags: runhidden; RunOnceId: "StopOverlay"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""TS LAN Overlay"""; Flags: runhidden; RunOnceId: "DelFirewallRule"

[Code]
{ The running program locks its exe; stop it before files are replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/f /im {#Exe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;
