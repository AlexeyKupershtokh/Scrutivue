param([switch]$FunctionsOnly, [switch]$Local)
$ErrorActionPreference = 'Stop'

function Get-CompilerCacheIdentity {
    param([System.Collections.IDictionary]$Toolchain)
    # Installation paths and Visual Studio installer patch versions are not
    # compiler identities. Keep actual Rust, C++ toolset and SDK changes isolated.
    $description = [ordered]@{ schema = 2 }
    foreach ($name in 'Rustc','Target','MsvcToolset','ClVersion','WindowsSdk') {
        $value = [string]$Toolchain[$name]
        if ([string]::IsNullOrWhiteSpace($value)) { throw "Missing compiler identity field: $name" }
        $description[$name] = $value.Trim().Replace("`r`n", "`n")
    }
    $bytes = [Text.Encoding]::UTF8.GetBytes(($description | ConvertTo-Json -Compress))
    return [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant()
}

if ($FunctionsOnly) { return }
if (!$Local -and (!$env:GITHUB_ENV -or !$env:GITHUB_OUTPUT)) { throw 'GitHub Actions environment files are required' }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if ($LASTEXITCODE -ne 0 -or !$installation) { throw 'Cannot find the Visual C++ x64 toolset' }
$developerCommand = Join-Path $installation 'Common7/Tools/VsDevCmd.bat'
# Use the measured toolchain for Cargo/cc too, rather than fingerprinting one
# installation while an implicit registry search chooses another compiler.
$environment = & $env:ComSpec /d /s /c "call `"$developerCommand`" -no_logo -arch=x64 -host_arch=x64 >nul && set"
if ($LASTEXITCODE -ne 0) { throw 'Cannot initialize the Visual C++ x64 environment' }
$developer = @{}
foreach ($line in $environment) {
    if ($line -match '^([^=]+)=(.*)$') { $developer[$Matches[1]] = $Matches[2] }
}
foreach ($name in 'PATH','INCLUDE','LIB','VCINSTALLDIR','VCToolsInstallDir','VCToolsVersion','WindowsSDKVersion','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH') {
    if (!$developer[$name]) { throw "Missing Visual C++ environment variable: $name" }
}
if ($developer.VSCMD_ARG_TGT_ARCH -ne 'x64' -or $developer.VSCMD_ARG_HOST_ARCH -ne 'x64') {
    throw 'The compiler cache requires an x64 host and target environment'
}
$names = 'PATH','INCLUDE','LIB','LIBPATH','VCINSTALLDIR','VSINSTALLDIR','VCToolsInstallDir','VCToolsVersion','WindowsSdkDir','WindowsSDKVersion','WindowsSDKLibVersion','UniversalCRTSdkDir','UCRTVersion','WindowsLibPath','VSCMD_ARG_TGT_ARCH','VSCMD_ARG_HOST_ARCH'
foreach ($name in $names) {
    if ($developer.ContainsKey($name)) {
        [Environment]::SetEnvironmentVariable($name, $developer[$name], 'Process')
        if (!$Local) { "$name=$($developer[$name])" | Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append }
    }
}
$compiler = (Get-Command cl.exe -CommandType Application).Source
if (!$compiler.StartsWith($env:VCToolsInstallDir, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The active C++ compiler does not belong to the selected toolset'
}
$rust = rustc --version --verbose | Out-String
if ($LASTEXITCODE -ne 0) { throw 'Cannot identify rustc' }
$toolchain = [ordered]@{
    Rustc = $rust.Trim()
    Target = 'x86_64-pc-windows-msvc'
    MsvcToolset = $env:VCToolsVersion.Trim()
    ClVersion = [Diagnostics.FileVersionInfo]::GetVersionInfo($compiler).FileVersion
    WindowsSdk = $env:WindowsSDKVersion.Trim().TrimEnd('\','/')
    CompilerPath = $compiler
    InstallationPath = $installation
}
$identity = Get-CompilerCacheIdentity $toolchain
$directory = Join-Path (Split-Path -Parent $PSScriptRoot) 'target'
New-Item -ItemType Directory -Path $directory -Force | Out-Null
$toolchain | ConvertTo-Json | Set-Content (Join-Path $directory 'ci-toolchain.json') -Encoding utf8
$toolchain | ConvertTo-Json | Write-Host
Write-Host "Compiler cache identity: $identity"
if (!$Local) { "identity=$identity" | Out-File -FilePath $env:GITHUB_OUTPUT -Encoding utf8 -Append }
