param(
    [string]$Version,
    [switch]$Publish,
    [string]$Token = $env:GITHUB_TOKEN
)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$packId = 'Polymer'
$channel = 'win'
$vpkVersion = '1.2.161'
$binaries = @('polymer-gui', 'polymer-watcher', 'polymer-injector')
$hooks = @(
    'polymer-hook-bluetoothapi-win32',
    'polymer-hook-d3d11',
    'polymer-hook-d3d12',
    'polymer-hook-dinput',
    'polymer-hook-dxgi',
    'polymer-hook-opengl32',
    'polymer-hook-sdl2',
    'polymer-hook-sdl3',
    'polymer-hook-vulkan-layer',
    'polymer-hook-win32',
    'polymer-hook-xinput'
)
$runtimes = @('SDL2.dll', 'SDL3.dll')

Push-Location $root
try {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $gui = $metadata.packages | Where-Object name -eq 'polymer-gui'
    if (-not $Version) { $Version = $gui.version }
    $repository = $gui.repository

    $installed = dotnet tool list -g | Select-String "^vpk\s+$([regex]::Escape($vpkVersion))\s"
    if (-not $installed) { dotnet tool update -g vpk --version $vpkVersion }

    $packages = ($binaries + $hooks) | ForEach-Object { '-p', $_ }
    cargo build --release @packages
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $release = Join-Path $root 'target\release'
    $stage = Join-Path $root 'target\velopack\stage'
    $output = Join-Path $root 'target\velopack\releases'
    Remove-Item $stage, $output -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $stage, $output | Out-Null
    $files = ($binaries | ForEach-Object { "$_.exe" }) +
        ($hooks | ForEach-Object { "$($_ -replace '-', '_').dll" }) +
        $runtimes
    foreach ($file in $files) {
        Copy-Item (Join-Path $release $file) $stage
    }

    if ($Token) {
        vpk download github --repoUrl $repository --token $Token --channel $channel -o $output
    }
    vpk pack --packId $packId --packVersion $Version --packTitle $packId --packAuthors 'SnowflakePowered' `
        --packDir $stage --mainExe 'polymer-gui.exe' --channel $channel --framework 'vcredist143-x64' -o $output
    if ($LASTEXITCODE -ne 0) { throw 'vpk pack failed' }

    if ($Publish) {
        if (-not $Token) { throw 'Publishing needs a GitHub token (-Token or GITHUB_TOKEN)' }
        vpk upload github --repoUrl $repository --token $Token --channel $channel -o $output `
            --releaseName "Polymer $Version" --tag "v$Version" --publish
        if ($LASTEXITCODE -ne 0) { throw 'vpk upload failed' }
    }
}
finally {
    Pop-Location
}
