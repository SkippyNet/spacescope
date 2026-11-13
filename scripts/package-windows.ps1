# Builds dist\SpaceScope-windows-x64.zip
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "build failed" }
$stage = "dist\SpaceScope-windows-x64"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\spacescope.exe, target\release\sscan.exe, README.md, CHANGELOG.md, LICENSE $stage
Compress-Archive -Path "$stage\*" -DestinationPath "$stage.zip" -Force
Remove-Item $stage -Recurse -Force
Get-Item "$stage.zip" | Select-Object Name, Length
