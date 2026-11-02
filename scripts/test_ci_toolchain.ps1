$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ci_toolchain.ps1') -FunctionsOnly
$baseline = @{
    Rustc = "rustc 1.99.1 (fixture)`ncommit-hash: fixture`nhost: x86_64-pc-windows-msvc"
    Target = 'x86_64-pc-windows-msvc'
    MsvcToolset = '14.51.12345'
    ClVersion = '19.51.12345.0'
    WindowsSdk = '10.0.26100.0'
    InstallationVersion = '18.11.12315.225'
    InstallationPath = 'C:\Visual Studio\first'
    CompilerPath = 'C:\Visual Studio\first\cl.exe'
}
$expected = Get-CompilerCacheIdentity $baseline
if ($expected -cnotmatch '^[0-9a-f]{64}$') { throw 'The identity is not a SHA-256 digest' }
$moved = $baseline.Clone()
$moved.InstallationVersion = '18.11.12328.437'
$moved.InstallationPath = 'D:\Another Visual Studio'
$moved.CompilerPath = 'D:\Another Visual Studio\cl.exe'
if ((Get-CompilerCacheIdentity $moved) -ne $expected) { throw 'Installer patches or paths changed the identity' }
$normalized = $baseline.Clone()
$normalized.Rustc = "  $($baseline.Rustc.Replace("`n", "`r`n"))`r`n"
if ((Get-CompilerCacheIdentity $normalized) -ne $expected) { throw 'Line endings changed the identity' }
foreach ($name in 'Rustc','Target','MsvcToolset','ClVersion','WindowsSdk') {
    $changed = $baseline.Clone()
    $changed[$name] += '-changed'
    if ((Get-CompilerCacheIdentity $changed) -eq $expected) { throw "Changing $name did not invalidate the identity" }
    foreach ($invalid in @($null, '', '   ')) {
        $changed = $baseline.Clone()
        $changed[$name] = $invalid
        $rejected = $false
        try { $null = Get-CompilerCacheIdentity $changed } catch { $rejected = $true }
        if (!$rejected) { throw "Missing $name was accepted" }
    }
}
Write-Host 'Compiler cache identity guards passed: installer/path stability, newline normalization, compiler/SDK invalidation and missing-field rejection.'
