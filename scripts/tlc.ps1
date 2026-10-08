param(
    [Parameter(Mandatory = $true)][string]$Spec,
    [string]$Config,
    [string[]]$Extra = @()
)

$tlaHome = if ($env:TLA_HOME) { $env:TLA_HOME } else { 'D:\Tooling\TLA+' }
$java = Get-ChildItem -Recurse -Filter java.exe (Join-Path $tlaHome 'plugins') | Select-Object -First 1 -ExpandProperty FullName
$classpath = @((Join-Path $tlaHome 'tla2tools.jar'), (Join-Path $tlaHome 'CommunityModules-deps.jar')) -join ';'

$workers = if ($env:TLC_WORKERS) { $env:TLC_WORKERS } else { 'auto' }
$specPath = Resolve-Path $Spec
if (-not $Config) { $Config = [IO.Path]::ChangeExtension($specPath, '.cfg') }
$metadir = Join-Path ([IO.Path]::GetTempPath()) ("tlc-" + [IO.Path]::GetFileNameWithoutExtension($specPath) + "-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))

Push-Location (Split-Path $specPath)
try {
    & $java -XX:+UseParallelGC -cp $classpath tlc2.TLC -config $Config -workers $workers -metadir $metadir -cleanup -noGenerateSpecTE @Extra (Split-Path -Leaf $specPath)
    $code = $LASTEXITCODE
} finally {
    Pop-Location
    Remove-Item -Recurse -Force $metadir -ErrorAction SilentlyContinue
}
exit $code
