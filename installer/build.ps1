# Builds the plugin, the overlay program and both installers into dist\.
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot)
cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
foreach ($iss in "plugin.iss", "overlay.iss") {
    & $iscc "/DAppVersion=$version" "installer\$iss"
    if ($LASTEXITCODE) { exit $LASTEXITCODE }
}
Copy-Item "Инструкция по установке.txt" dist\
