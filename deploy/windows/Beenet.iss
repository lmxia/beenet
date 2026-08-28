#define MyAppName "Beenet"
#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#ifndef BeenetWorkerExe
  #define BeenetWorkerExe "..\..\target\release\beenet-worker.exe"
#endif
#ifndef BeenetAppExe
  #define BeenetAppExe "..\..\target\release\Beenet.exe"
#endif
#ifndef BeenetOutputDir
  #define BeenetOutputDir "..\..\out\windows"
#endif

[Setup]
AppId={{8F3C1E2A-7B64-4D91-9C18-A1B2C3D4E5F6}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher=Beenet
DefaultDirName={localappdata}\Programs\Beenet
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir={#BeenetOutputDir}
OutputBaseFilename=BeenetSetup-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
UninstallDisplayName={#MyAppName}
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Default.isl"

[LangOptions]
LanguageName=Simplified Chinese
LanguageID=$0804
LanguageCodePage=0

[Messages]
ButtonBack=< 上一步(&B)
ButtonNext=下一步(&N) >
ButtonInstall=安装(&I)
ButtonCancel=取消
ButtonFinish=完成(&F)
ButtonYes=是(&Y)
ButtonNo=否(&N)
ButtonNewFolder=新建文件夹(&M)
ClickNext=单击「下一步」继续，或单击「取消」退出安装程序。
ClickInstall=单击「安装」开始安装，或单击「上一步」查看/更改设置。
ClickFinish=单击「完成」退出安装程序。
SelectDirDesc=选择安装位置
SelectDirLabel3=安装程序会把 [name] 装到下面的文件夹。
SelectDirBrowseLabel=继续请单击「下一步」。若要选择其他文件夹，请单击「浏览」。
WizardSelectDir=选择目标位置
WizardSelectTasks=选择附加任务
SelectTasksDesc=选择要执行的附加任务
SelectTasksLabel2=选择要在安装 [name] 时执行的附加任务，然后单击「下一步」。
WizardReady=准备安装
ReadyLabel1=安装程序准备开始安装 [name]。
ReadyLabel2a=单击「安装」继续。若要查看或更改设置，请单击「上一步」。
WizardInstalling=正在安装
InstallingLabel=正在安装 [name]，请稍候。
WizardFinished=安装完成
FinishedHeadingLabel=完成 [name] 安装向导
FinishedLabelNoIcons=安装程序已将 [name] 安装完成。
FinishedLabel=安装程序已将 [name] 安装完成。可以通过开始菜单或桌面快捷方式打开应用。
StatusExtractFiles=正在解压文件...
StatusCreateIcons=正在创建快捷方式...
CreateDesktopIcon=创建桌面快捷方式(&D)

[CustomMessages]
CreateDesktopIcon=创建桌面快捷方式(&D)
AdditionalIcons=附加图标：

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#BeenetAppExe}"; DestDir: "{app}"; DestName: "Beenet.exe"; Flags: ignoreversion
Source: "{#BeenetWorkerExe}"; DestDir: "{app}"; DestName: "beenet-worker.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\Beenet.exe"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\Beenet.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\Beenet.exe"; Description: "启动 Beenet"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
Type: files; Name: "{userappdata}\beenet\ui-state.json"

[Code]
var
  CacheDirPage: TInputDirWizardPage;

function TomlEscape(const Value: String): String;
begin
  Result := Value;
  StringChangeEx(Result, '\', '\\', True);
  StringChangeEx(Result, '"', '\"', True);
end;

function ConfigPath: String;
begin
  Result := ExpandConstant('{userappdata}\beenet\config.toml');
end;

function DefaultCacheDir: String;
begin
  Result := ExpandConstant('{localappdata}\beenet\wasm_cache');
end;

procedure InitializeWizard;
begin
  CacheDirPage := CreateInputDirPage(
    wpSelectDir,
    '缓存目录',
    '选择 Worker 缓存和身份文件存放位置。',
    'Wasm 缓存和 identity.key 会写在这个目录。节点名称和地区请在安装完成后打开应用再填写。',
    False,
    ''
  );
  CacheDirPage.Add('');
  CacheDirPage.Values[0] := DefaultCacheDir;
end;

function CacheDirValue: String;
begin
  Result := Trim(CacheDirPage.Values[0]);
  if Result = '' then
    Result := DefaultCacheDir;
end;

procedure WriteWorkerConfig;
var
  Path, CacheDir, Contents: String;
begin
  Path := ConfigPath;
  CacheDir := CacheDirValue;
  ForceDirectories(ExtractFileDir(Path));
  ForceDirectories(CacheDir);
  if FileExists(Path) then
    Exit;
  Contents :=
    '[worker]' + #13#10 +
    'backend = "native"' + #13#10 +
    'listen_addr = "/ip4/0.0.0.0/tcp/0"' + #13#10 +
    'registry_url = "http://registry.hyperos.online"' + #13#10 +
    'wasm_fetch_base = "http://cloud.hyperos.online/api/v1/artifacts"' + #13#10 +
    'wasm_fetch_timeout_secs = 60' + #13#10 +
    'registry_heartbeat_secs = 30' + #13#10 +
    'wasm_cache_dir = "' + TomlEscape(CacheDir) + '"' + #13#10 +
    #13#10 +
    '[worker.quota]' + #13#10 +
    'cpu_percent = 25' + #13#10 +
    'memory_mb = 512' + #13#10 +
    'pids_max = 128' + #13#10;
  SaveStringToFile(Path, Contents, False);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    WriteWorkerConfig;
end;
