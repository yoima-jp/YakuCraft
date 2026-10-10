$ErrorActionPreference = 'Stop'
$OutputEncoding = [Console]::OutputEncoding = [Console]::InputEncoding = [System.Text.UTF8Encoding]::new()

$programFilesX86 = [Environment]::GetFolderPath('ProgramFilesX86')
$programFiles = [Environment]::GetFolderPath('ProgramFiles')
$applicationDirectories = @(
    (Join-Path $programFilesX86 'Microsoft\EdgeWebView\Application'),
    (Join-Path $programFiles 'Microsoft\EdgeWebView\Application'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft\EdgeWebView\Application')
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
$versions = foreach ($applicationDirectory in $applicationDirectories) {
    foreach ($versionDirectory in Get-ChildItem -LiteralPath $applicationDirectory -Directory) {
        if (($versionDirectory.Name -match '^\d+\.\d+\.\d+\.\d+$') -and (Test-Path -LiteralPath (Join-Path $versionDirectory.FullName 'msedgewebview2.exe'))) {
            [pscustomobject]@{ Text = $versionDirectory.Name; Parsed = [version]$versionDirectory.Name }
        }
    }
}

$runtime = $versions | Sort-Object Parsed -Descending | Select-Object -First 1
if (-not $runtime) {
    throw 'インストール済みの WebView2 Runtime が見つかりません。'
}

$cacheDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) '.cache\msedgedriver'
$versionDirectory = Join-Path $cacheDirectory $runtime.Text
$driverPath = Join-Path $versionDirectory 'msedgedriver.exe'
if (-not (Test-Path -LiteralPath $driverPath)) {
    New-Item -ItemType Directory -Path $versionDirectory -Force | Out-Null
    $archivePath = Join-Path $versionDirectory 'edgedriver.zip'
    $archiveUrl = "https://msedgedriver.microsoft.com/$($runtime.Text)/edgedriver_win64.zip"
    Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath
    Expand-Archive -LiteralPath $archivePath -DestinationPath $versionDirectory -Force
    Remove-Item -LiteralPath $archivePath -Force
}

if (-not (Test-Path -LiteralPath $driverPath)) {
    throw "Edge WebDriver を展開できませんでした: $driverPath"
}

Write-Output (Resolve-Path -LiteralPath $driverPath).Path

