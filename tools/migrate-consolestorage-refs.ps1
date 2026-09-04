<#
.SYNOPSIS
Migrates project-named console-storage Git refs to a stable namespace.

.DESCRIPTION
Run this once, with Polymer stopped, against the console-storage root that
contains the platforms directory. The migration renames refs/snowglass/* and
refs/polymer/* to refs/consoles/*, and their refs/notes equivalents to
refs/notes/consoles/*. Existing reflogs are preserved. This script does not
move the storage directory itself; pass the storage root that Polymer will use.

.EXAMPLE
./tools/migrate-consolestorage-refs.ps1 -ConsoleStorageRoot C:\path\to\storage
#>

[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory, Position = 0)]
    [string] $ConsoleStorageRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path -LiteralPath $ConsoleStorageRoot).Path
$platforms = Join-Path $root 'platforms'
if (-not (Test-Path -LiteralPath $platforms -PathType Container)) {
    throw "Console-storage root does not contain a platforms directory: $root"
}

$repositories = @(
    Get-ChildItem -LiteralPath $platforms -Directory |
        Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName '.git') -PathType Container }
)
if ($repositories.Count -eq 0) {
    throw "No console-storage Git repositories found under: $platforms"
}

foreach ($repository in $repositories) {
    $path = $repository.FullName
    $sources = @(
        & git -C $path for-each-ref '--format=%(refname)' `
            'refs/snowglass/' 'refs/polymer/' `
            'refs/notes/snowglass/' 'refs/notes/polymer/'
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Could not enumerate refs in: $path"
    }
    if ($sources.Count -eq 0) {
        Write-Host "No project-named console-storage refs in $($repository.Name); skipping."
        continue
    }

    $migration = foreach ($source in $sources) {
        $target = if ($source.StartsWith('refs/snowglass/')) {
            'refs/consoles/' + $source.Substring('refs/snowglass/'.Length)
        } elseif ($source.StartsWith('refs/polymer/')) {
            'refs/consoles/' + $source.Substring('refs/polymer/'.Length)
        } elseif ($source.StartsWith('refs/notes/snowglass/')) {
            'refs/notes/consoles/' + $source.Substring('refs/notes/snowglass/'.Length)
        } else {
            'refs/notes/consoles/' + $source.Substring('refs/notes/polymer/'.Length)
        }
        $sourceObject = (& git -C $path rev-parse --verify $source).Trim()
        if ($LASTEXITCODE -ne 0) {
            throw "Could not resolve $source in: $path"
        }
        $targetObject = & git -C $path rev-parse --verify $target 2>$null
        if ($LASTEXITCODE -eq 0 -and $targetObject.Trim() -ne $sourceObject) {
            throw "Ref collision in $path`: $source and $target point to different objects"
        }
        [pscustomobject]@{
            Source = $source
            Target = $target
            Object = $sourceObject
        }
    }

    foreach ($targetGroup in $migration | Group-Object Target) {
        if ($targetGroup.Count -gt 1) {
            $conflictingSources = $targetGroup.Group.Source -join ', '
            throw "Ref collision in $path`: $conflictingSources both map to $($targetGroup.Name)"
        }
    }

    if (-not $PSCmdlet.ShouldProcess($path, "migrate $($migration.Count) console-storage ref(s)")) {
        continue
    }

    foreach ($entry in $migration) {
        $sourceLog = (& git -C $path rev-parse --git-path "logs/$($entry.Source)").Trim()
        $targetLog = (& git -C $path rev-parse --git-path "logs/$($entry.Target)").Trim()
        if (Test-Path -LiteralPath $sourceLog -PathType Leaf) {
            $targetLogParent = Split-Path -Parent $targetLog
            New-Item -ItemType Directory -Path $targetLogParent -Force | Out-Null
            Copy-Item -LiteralPath $sourceLog -Destination $targetLog -Force
        }

        & git -C $path update-ref --create-reflog $entry.Target $entry.Object
        if ($LASTEXITCODE -ne 0) {
            throw "Could not create $($entry.Target) in: $path"
        }
        & git -C $path update-ref -d $entry.Source $entry.Object
        if ($LASTEXITCODE -ne 0) {
            throw "Created $($entry.Target), but could not delete $($entry.Source) in: $path"
        }
        Write-Host "$($entry.Source) -> $($entry.Target)"
    }
}
