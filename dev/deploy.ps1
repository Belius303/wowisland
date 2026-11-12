# Compile, remplace l'île en cours par la nouvelle version, puis supprime les anciennes builds.
#   pwsh dev/deploy.ps1            → build + déploiement + ménage
#   pwsh dev/deploy.ps1 -CleanOnly → ménage seulement
# On garde : target\release\dynamic-island.exe (lancé au démarrage de Windows)
#            target\build-new\release (cache de compilation : sans lui, chaque build prend 10 min au lieu de 2)
#
# ATTENTION : lancé depuis le terminal de l'appli Claude (appli MSIX), ce script relance l'île DANS le conteneur
# de Claude : ses écritures dans AppData et HKCU sont redirigées vers
# %LOCALAPPDATA%\Packages\Claude_*\LocalCache (invisibles pour le reste de Windows, ex. Spotifast).
# Les données de l'île sont donc dans %USERPROFILE%\.dynamicisland (jamais redirigé). Le journal de l'île
# signale ce cas (« ATTENTION : île lancée depuis l'appli empaquetée… »). Pour une île 100 % normale,
# la relancer soi-même (menu Démarrer, double-clic sur l'exe ou redémarrage du PC).
param([switch]$CleanOnly)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root
$exe = Join-Path $root 'target\release\dynamic-island.exe'

if (-not $CleanOnly) {
    cargo build --release --target-dir target/build-new
    if ($LASTEXITCODE -ne 0) { throw "build échouée" }
    Get-Process dynamic-island -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 800
    New-Item -ItemType Directory -Force (Split-Path $exe) | Out-Null
    Copy-Item target\build-new\release\dynamic-island.exe $exe -Force
    Start-Process $exe
}

function Size($p) { (Get-ChildItem $p -Recurse -Force -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum }
$before = Size target

# anciennes builds complètes (debug, tests, ancienne release) : seul l'exe déployé reste
foreach ($d in 'target\debug', 'target\build-new\debug') {
    if (Test-Path $d) { Remove-Item $d -Recurse -Force -ErrorAction SilentlyContinue }
}
Get-ChildItem target\release -Force -ErrorAction SilentlyContinue | Where-Object { $_.FullName -ne $exe } |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue

# dans le cache : les versions précédentes de l'île (une par build) ; on ne garde que la dernière
$deps = 'target\build-new\release\deps'
if (Test-Path $deps) {
    $mine = Get-ChildItem $deps -File -Filter 'dynamic_island-*' | Sort-Object LastWriteTime -Descending
    $latest = $mine | Select-Object -First 1
    if ($latest) {
        $keep = $latest.BaseName
        $mine | Where-Object { $_.BaseName -ne $keep } | Remove-Item -Force -ErrorAction SilentlyContinue
    }
}

$after = Size target
"ménage : {0:N0} Mo libérés (target = {1:N0} Mo)" -f (($before - $after) / 1MB), ($after / 1MB)
