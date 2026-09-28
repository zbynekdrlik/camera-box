# camera-box issue 1386: the A/V-sync dock output's C++ sources read as ONE text, for the pwsh
# anchor steps of BOTH .github/workflows/windows-genlock.yml and windows-genlock-fast.yml.
#
# The output is split into sync-test-output.cpp (the obs_output_info callbacks),
# sync-test-output-video.cpp, sync-test-output-audio.cpp and the shared
# sync-test-output-internal.hpp. A step dot-sources this file and reads the union, so a function
# moving between those files never breaks an anchor:
#
#   . ./vendor/av-sync-dock/test/dock-output-source.ps1
#   $output = Get-DockOutputSource
#   $body = Get-DockBody $output 'static void st_stop(void *data, uint64_t)'
#
# Twin of tests/support/av_sync_dock_output.rs: the same files (sync-test-output.cpp and every
# sync-test-output-*.cpp / -*.hpp) in the same ordinal order; change both together. The public
# sync-test-output.hpp (the dock UI's interface) is not part of it.

$AvSyncDockOutputDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../src'))

function Get-DockOutputFiles {
  $names = [string[]]@(Get-ChildItem -LiteralPath $AvSyncDockOutputDir -File | ForEach-Object { $_.Name } | Where-Object {
      $_ -ceq 'sync-test-output.cpp' -or ($_.StartsWith('sync-test-output-', [StringComparison]::Ordinal) -and
        ($_.EndsWith('.cpp', [StringComparison]::Ordinal) -or $_.EndsWith('.hpp', [StringComparison]::Ordinal)))
    })
  [Array]::Sort($names, [StringComparer]::Ordinal)
  if ($names -cnotcontains 'sync-test-output.cpp') {
    Write-Error "dock output sources: sync-test-output.cpp is gone from $AvSyncDockOutputDir"; exit 1
  }
  return $names
}

# Every output file's text, in Get-DockOutputFiles order, whitespace collapsed to one space (the
# `-replace '\s+', ' '` every anchor step applies). Comments are kept.
function Get-DockOutputSource {
  $texts = foreach ($name in (Get-DockOutputFiles)) { Get-Content -LiteralPath (Join-Path $AvSyncDockOutputDir $name) -Raw }
  return (($texts -join "`n") -replace '\s+', ' ')
}

# The text from the signature through its brace-balanced body. The signature must occur exactly once
# in $src: a second match would silently pick one function of two.
function Get-DockBody([string]$src, [string]$sig) {
  $i = $src.IndexOf($sig, [StringComparison]::Ordinal)
  if ($i -lt 0) {
    Write-Error "dock output sources ($((Get-DockOutputFiles) -join ', ')): '$sig' not found"; exit 1
  }
  if ($src.IndexOf($sig, $i + 1, [StringComparison]::Ordinal) -ge 0) {
    Write-Error "dock output sources: '$sig' occurs more than once -- a body anchor must name one function"; exit 1
  }
  $depth = 0
  for ($k = $src.IndexOf('{', $i + $sig.Length); $k -ge 0 -and $k -lt $src.Length; $k++) {
    if ($src[$k] -eq '{') { $depth++ }
    elseif ($src[$k] -eq '}') { $depth--; if ($depth -eq 0) { return $src.Substring($i, $k + 1 - $i) } }
  }
  Write-Error "dock output sources: unbalanced body for '$sig'"; exit 1
}
