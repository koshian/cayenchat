<#
Builds a throw-away MSIX of cayenchat.exe, to check the packaged ("StartupTask")
login-startup backend of #153. It is a test subject, not a release package.

  .\scripts\msix-test\build.ps1 -Exe target\release\cayenchat.exe [-Install]

Needs the Windows SDK (makeappx.exe, signtool.exe) on PATH or in the default
SDK folder. The package is signed with a new self-signed certificate; -Install
trusts that certificate for the local machine, so run it from an elevated
PowerShell, then installs the package for the current user.

Output goes to target\tmp\msix-test. Remove with:
  Get-AppxPackage CayenChat.Test | Remove-AppxPackage
#>
param(
    [Parameter(Mandatory)][string]$Exe,
    [switch]$Install
)
$ErrorActionPreference = 'Stop'

$Identity = 'CayenChat.Test'
$Subject = 'CN=CayenChat Test'
$Out = Join-Path $PSScriptRoot '..\..\target\tmp\msix-test' | ForEach-Object { [IO.Path]::GetFullPath($_) }
$Layout = Join-Path $Out 'layout'
Remove-Item $Out -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path (Join-Path $Layout 'Assets') | Out-Null

function Find-SdkTool($name) {
    $found = Get-Command $name -ErrorAction SilentlyContinue
    if ($found) { return $found.Source }
    $tool = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin" -Recurse -Filter $name -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match "\\$($env:PROCESSOR_ARCHITECTURE -replace 'AMD64','x64')\\" } |
        Sort-Object FullName -Descending | Select-Object -First 1
    if (-not $tool) { throw "$name not found; install the Windows SDK" }
    return $tool.FullName
}

Copy-Item $Exe (Join-Path $Layout 'cayenchat.exe')

# Plain placeholder logos: the manifest requires the files to exist.
Add-Type -AssemblyName System.Drawing
foreach ($logo in @{ 'StoreLogo.png' = 50; 'Square44x44Logo.png' = 44; 'Square150x150Logo.png' = 150 }.GetEnumerator()) {
    $bitmap = New-Object System.Drawing.Bitmap $logo.Value, $logo.Value
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.Clear([System.Drawing.Color]::FromArgb(0xC0, 0x39, 0x2B))
    $graphics.Dispose()
    $bitmap.Save((Join-Path $Layout "Assets\$($logo.Key)"), [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
}

$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$manifest = Get-Content (Join-Path $PSScriptRoot 'AppxManifest.xml') -Raw
$manifest = $manifest.Replace('@IDENTITY@', $Identity).Replace('@SUBJECT@', $Subject).Replace('@ARCH@', $arch)
Set-Content (Join-Path $Layout 'AppxManifest.xml') $manifest -Encoding utf8

$package = Join-Path $Out 'CayenChat.Test.msix'
& (Find-SdkTool 'makeappx.exe') pack /d $Layout /p $package /nv
if ($LASTEXITCODE) { throw 'makeappx failed' }

$cert = New-SelfSignedCertificate -Type Custom -Subject $Subject -KeyUsage DigitalSignature `
    -FriendlyName 'CayenChat Test' -CertStoreLocation 'Cert:\CurrentUser\My' `
    -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
$pfx = Join-Path $Out 'test.pfx'
$password = ConvertTo-SecureString -String ([guid]::NewGuid().ToString()) -Force -AsPlainText
Export-PfxCertificate -Cert $cert -FilePath $pfx -Password $password | Out-Null
$plain = [Runtime.InteropServices.Marshal]::PtrToStringAuto([Runtime.InteropServices.Marshal]::SecureStringToBSTR($password))
& (Find-SdkTool 'signtool.exe') sign /fd SHA256 /a /f $pfx /p $plain $package
if ($LASTEXITCODE) { throw 'signtool failed' }
Export-Certificate -Cert $cert -FilePath (Join-Path $Out 'test.cer') | Out-Null
Remove-Item $pfx
Remove-Item "Cert:\CurrentUser\My\$($cert.Thumbprint)"

Write-Host "Built $package"
if ($Install) {
    Import-Certificate -FilePath (Join-Path $Out 'test.cer') -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople' | Out-Null
    Add-AppxPackage $package
    Write-Host 'Installed. Start "CayenChat Test" from the Start menu.'
}
