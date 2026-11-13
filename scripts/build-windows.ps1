<#
.SYNOPSIS
    在 Windows 上原生构建 Switcheroo 的 Windows 版本（默认 msvc 目标）。

.DESCRIPTION
    编译 switcheroo-app 的 dist profile，把 exe 与 README.txt 打进 dist\switcheroo-win-x64\，
    并生成 dist\switcheroo-win-x64.zip。

    图标资源由 crates\app\build.rs 处理：msvc 目标用 rc.exe（Windows SDK / VS 开发者环境），
    找不到时退到 llvm-rc；两者都没有时只警告，exe 使用默认图标。

.PARAMETER NoPackage
    只编译，不组装 dist 目录、不生成压缩包。

.PARAMETER Target
    Rust 目标三元组，默认 x86_64-pc-windows-msvc。

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1

.EXAMPLE
    powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1 -NoPackage
#>
[CmdletBinding()]
param(
    [switch]$NoPackage,
    [string]$Target = "x86_64-pc-windows-msvc"
)

$ErrorActionPreference = "Stop"

# 全部路径从脚本所在目录推导，不写死用户名或绝对路径。
$Root = Split-Path -Parent $PSScriptRoot
$Package = "switcheroo-app"
$BinName = "switcheroo"
$DistName = "switcheroo-win-x64"
$DistDir = Join-Path $Root "dist"
$PkgDir = Join-Path $DistDir $DistName
$ExePath = Join-Path (Join-Path (Join-Path (Join-Path $Root "target") $Target) "dist") ($BinName + ".exe")
$ZipPath = Join-Path $DistDir ($DistName + ".zip")
$TotalSteps = 5

Write-Host "==> 构建 Windows 版本（$Target，profile=dist）"
Write-Host "    仓库根目录：$Root"
Write-Host ""

# ---- [1/5] 检查 cargo ----
Write-Host "[1/$TotalSteps] 检查 cargo ..."
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if ($null -eq $cargo) {
    Write-Host "❌ 未找到 cargo，请先安装 Rust（https://rustup.rs），然后重新运行本脚本。"
    exit 1
}
Write-Host "    ✅ 找到：$($cargo.Source)"

# ---- [2/5] 检查资源编译器 ----
Write-Host "[2/$TotalSteps] 检查资源编译器（rc.exe / llvm-rc）..."
$resourceCompiler = $null
if ($env:RC) {
    $resourceCompiler = $env:RC
} elseif ($env:WINDRES) {
    $resourceCompiler = $env:WINDRES
} else {
    foreach ($candidate in @("rc.exe", "llvm-rc")) {
        $found = Get-Command $candidate -ErrorAction SilentlyContinue
        if ($null -ne $found) {
            $resourceCompiler = $found.Source
            break
        }
    }
}
if ($null -ne $resourceCompiler) {
    Write-Host "    ✅ 找到：$resourceCompiler"
} else {
    Write-Host "    ⚠️  未找到 rc.exe / llvm-rc：exe 将没有自定义图标（编译继续，不影响运行）。"
    Write-Host "       在 VS 开发者命令提示符里运行可自动带上 rc.exe。"
}

# ---- [3/5] 编译 ----
Write-Host "[3/$TotalSteps] 编译（cargo build --profile dist）..."
& cargo build --profile dist -p $Package --target $Target
if ($LASTEXITCODE -ne 0) {
    Write-Host "❌ 编译失败（cargo 退出码 $LASTEXITCODE），见上方输出。"
    exit 1
}
if (-not (Test-Path $ExePath)) {
    Write-Host "❌ 编译结束但找不到产物：$ExePath"
    exit 1
}
Write-Host "    ✅ 产物：$ExePath"

# ---- [4/5] 打印体积 ----
Write-Host "[4/$TotalSteps] 检查产物 ..."
$exeSize = (Get-Item $ExePath).Length
Write-Host ("    exe 体积：{0:N1} MB（{1:N0} 字节）" -f ($exeSize / 1MB), $exeSize)

# ---- [5/5] 组装 + 打包 ----
if ($NoPackage) {
    Write-Host "[5/$TotalSteps] 跳过打包（-NoPackage）"
    Write-Host ""
    Write-Host "✅ 完成（未打包）。exe：$ExePath"
    exit 0
}

Write-Host "[5/$TotalSteps] 组装 dist\$DistName ..."
if (Test-Path $PkgDir) {
    Remove-Item -Recurse -Force $PkgDir
}
New-Item -ItemType Directory -Force -Path $PkgDir | Out-Null
Copy-Item -Path $ExePath -Destination (Join-Path $PkgDir ($BinName + ".exe")) -Force

$readme = Join-Path $Root "README.md"
if (Test-Path $readme) {
    Copy-Item -Path $readme -Destination (Join-Path $PkgDir "README.txt") -Force
    Write-Host "    已复制 README.md -> README.txt"
} else {
    Write-Host "    ⚠️  未找到 README.md，跳过说明文件。"
}

# Compress-Archive 是 PowerShell 5.1 自带的，无需 7-Zip 等外部工具。
try {
    if (Test-Path $ZipPath) {
        Remove-Item -Force $ZipPath
    }
    Compress-Archive -Path $PkgDir -DestinationPath $ZipPath -Force
} catch {
    Write-Host "❌ 生成压缩包失败：$($_.Exception.Message)"
    exit 1
}
$zipSize = (Get-Item $ZipPath).Length
Write-Host ("    ✅ 压缩包：{0}（{1:N1} MB）" -f $ZipPath, ($zipSize / 1MB))
Write-Host ""
Write-Host "✅ 完成。交付目录：$PkgDir"
exit 0
