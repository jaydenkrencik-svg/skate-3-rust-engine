param(
    # Faster iteration: the dev profile instead of wasm-release (fat LTO).
    [switch]$Dev,
    [string]$TargetDirectory = (Join-Path (Split-Path $PSScriptRoot -Parent) 'target'),
    [string]$OutDirectory = (Join-Path $PSScriptRoot 'dist')
)
# Builds the browser package into web/dist: the loader page, build.json and the
# wasm-bindgen output in pkg/. Requires the wasm32-unknown-unknown target and
# wasm-bindgen-cli matching the wasm-bindgen crate (0.2.127). Runs wasm-opt
# when it is on PATH. Serve the result with skate-web-server (README-web.md).
$ProjectRoot = Split-Path $PSScriptRoot -Parent
$ErrorActionPreference = 'Stop'
. (Join-Path $ProjectRoot 'scripts/Ensure-Rust.ps1')
$profile = if ($Dev) { 'dev' } else { 'wasm-release' }
$profileDirectory = if ($Dev) { 'debug' } else { 'wasm-release' }
$sw = [Diagnostics.Stopwatch]::StartNew()
Push-Location $ProjectRoot
try {
    $bindgenVersion = (& wasm-bindgen --version 2>$null)
    if ($bindgenVersion -notmatch '0\.2\.127') {
        throw "wasm-bindgen-cli 0.2.127 is required (found '$bindgenVersion'). Install it with: cargo install wasm-bindgen-cli --version 0.2.127 --locked"
    }
    # No default features: Bevy's dynamic_linking does not exist on wasm.
    & cargo build --locked -p skate-game --bin skate3rust --no-default-features `
        --target wasm32-unknown-unknown --profile $profile --target-dir $TargetDirectory
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }

    $wasm = Join-Path $TargetDirectory "wasm32-unknown-unknown/$profileDirectory/skate3rust.wasm"
    if (Test-Path -LiteralPath $OutDirectory) { Remove-Item -LiteralPath $OutDirectory -Recurse -Force }
    $pkg = Join-Path $OutDirectory 'pkg'
    New-Item -ItemType Directory -Force -Path $pkg | Out-Null
    & wasm-bindgen --target web --no-typescript --out-dir $pkg $wasm
    if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed ($LASTEXITCODE)" }

    $bindgenWasm = Join-Path $pkg 'skate3rust_bg.wasm'
    if (-not $Dev -and (Get-Command wasm-opt -ErrorAction SilentlyContinue)) {
        & wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext `
            --enable-mutable-globals --enable-reference-types --enable-multivalue `
            -o $bindgenWasm $bindgenWasm
        if ($LASTEXITCODE -ne 0) { throw "wasm-opt failed ($LASTEXITCODE)" }
    } elseif (-not $Dev) {
        Write-Host 'wasm-opt not found; skipping (install binaryen for a smaller download)'
    }

    Copy-Item -Path (Join-Path $PSScriptRoot 'static/*') -Destination $OutDirectory -Recurse
    $revision = (& git rev-parse HEAD 2>$null)
    $dirty = [bool](& git status --porcelain --untracked-files=no 2>$null)
    @{ revision = "$revision"; dirty = $dirty; profile = $profile } |
        ConvertTo-Json | Set-Content -Encoding utf8 -LiteralPath (Join-Path $OutDirectory 'build.json')

    $size = (Get-Item -LiteralPath $bindgenWasm).Length / 1MB
    Write-Host ("Browser package: {0} ({1:N1} MB wasm) in {2:N0}s" -f $OutDirectory, $size, $sw.Elapsed.TotalSeconds)
} finally {
    Pop-Location
}
