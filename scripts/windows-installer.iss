; Setup program for Kubernaut, built by scripts\bundle-windows.ps1 -Installer:
;   ISCC.exe /DAppVersion=1.2.3 scripts\windows-installer.iss
; It installs for the current user, so nothing needs administrator rights, and
; the app can then replace its own files when it updates.

#ifndef AppVersion
  #error Pass the version: ISCC.exe /DAppVersion=1.2.3
#endif

[Setup]
; Fixed for good: it is how a later setup finds this install and upgrades it.
AppId={{6F0C2B7E-5D4A-4B7C-9E1D-3A8F2C5B7D10}
AppName=Kubernaut
AppVersion={#AppVersion}
AppPublisher=Kubernaut
AppPublisherURL=https://github.com/terdiam/kubernaut
DefaultDirName={autopf}\Kubernaut
DefaultGroupName=Kubernaut
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\crates\kubernaut-ui\assets\icon.ico
UninstallDisplayIcon={app}\kubernaut-ui.exe
OutputDir=..\dist
OutputBaseFilename=Kubernaut-{#AppVersion}-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: "prerequisites"; Description: "Install &kubectl and the OIDC login plugin if missing (downloads about 70 MB)"

[Files]
Source: "..\dist\Kubernaut\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Kubernaut"; Filename: "{app}\kubernaut-ui.exe"
Name: "{autodesktop}\Kubernaut"; Filename: "{app}\kubernaut-ui.exe"; Tasks: desktopicon

[Run]
; Before the app starts, so a kubeconfig that logs in through OIDC works the first time.
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\install-prerequisites.ps1"""; StatusMsg: "Installing kubectl and the OIDC login plugin..."; Flags: runhidden waituntilterminated; Tasks: prerequisites
Filename: "{app}\kubernaut-ui.exe"; Description: "Start Kubernaut"; Flags: nowait postinstall skipifsilent shellexec
