<#
.SYNOPSIS
  Install and launch a TGZTerminal Windows build, and fail unless it really
  opens a window and stays up.

.DESCRIPTION
  Every Windows startup failure this project has shipped looked the same to a
  user: a busy cursor, then nothing. A GUI-subsystem process has no console,
  so nothing in CI noticed either. This script is the gate against that:

    * installs the Setup exe silently (or uses an extracted portable tree),
    * launches wezterm-gui.exe with logging on,
    * requires a visible top-level window of the terminal's own window class
      within -WindowTimeout seconds, and the process still alive
      -StaySeconds later,
    * fails on any console window that appears meanwhile (the "ten cmd
      windows at startup" bug), on an error dialog, and on a panic in the log,
    * requires each -ExpectLog regex to match the log,
  and always leaves a screenshot, the logs and a session probe in -OutDir.

  The hosted runners have no GPU (OpenGL there is GDI Generic 1.1), so a
  default run also exercises the renderer fallback to ANGLE.
#>
param(
    # Setup exe to install. Mutually exclusive with -AppDir.
    [string]$Installer,
    # Directory that already holds wezterm-gui.exe (a portable extraction).
    [string]$AppDir,
    # Lua config to run with (sets WEZTERM_CONFIG_FILE).
    [string]$Config,
    # Regexes the log must match, e.g. 'OpenGL: '.
    [string[]]$ExpectLog = @(),
    [string]$OutDir = (Join-Path $env:RUNNER_TEMP 'tgz-smoke'),
    [int]$WindowTimeout = 60,
    [int]$StaySeconds = 20,
    # How long past -StaySeconds to keep it running while -ExpectLog patterns
    # are still unmatched: background work (a WSL probe while the distro's VM
    # boots) can legitimately take a while to report.
    [int]$ExpectTimeout = 90
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$failures = New-Object System.Collections.Generic.List[string]

Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class TgzWin {
    delegate bool EnumProc(IntPtr hwnd, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc cb, IntPtr lparam);
    [DllImport("user32.dll")] static extern bool EnumChildWindows(IntPtr parent, EnumProc cb, IntPtr lparam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr hwnd, StringBuilder buf, int max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr hwnd, StringBuilder buf, int max);

    public class Win {
        public long Handle; public uint Pid; public string Class; public string Title;
        public override string ToString() { return string.Format("0x{0:x} pid={1} class={2} title={3}", Handle, Pid, Class, Title); }
    }

    static string ClassOf(IntPtr h) { var b = new StringBuilder(256); GetClassName(h, b, b.Capacity); return b.ToString(); }
    static string TitleOf(IntPtr h) { var b = new StringBuilder(1024); GetWindowText(h, b, b.Capacity); return b.ToString(); }

    public static List<Win> Visible() {
        var result = new List<Win>();
        EnumWindows((h, l) => {
            if (IsWindowVisible(h)) {
                uint pid; GetWindowThreadProcessId(h, out pid);
                result.Add(new Win { Handle = h.ToInt64(), Pid = pid, Class = ClassOf(h), Title = TitleOf(h) });
            }
            return true;
        }, IntPtr.Zero);
        return result;
    }

    // All text inside a window: a message box keeps its message in a child.
    public static string AllText(long handle) {
        var parts = new List<string> { TitleOf(new IntPtr(handle)) };
        EnumChildWindows(new IntPtr(handle), (h, l) => { var t = TitleOf(h); if (t.Length > 0) parts.Add(t); return true; }, IntPtr.Zero);
        return string.Join(" | ", parts);
    }
}
'@

function Write-Step([string]$message) { Write-Host "==> $message" }

# --- Session probe: whether this job can show windows at all ------------------
$users = try { (query user 2>&1 | Out-String).Trim() } catch { "query user unavailable: $_" }
$probe = [ordered]@{
    SessionId       = (Get-Process -Id $PID).SessionId
    UserInteractive = [Environment]::UserInteractive
    GlassSessionId  = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server' -Name GlassSessionId -ErrorAction SilentlyContinue).GlassSessionId
    RemoteSession   = [System.Windows.Forms.SystemInformation]::TerminalServerSession
    Screen          = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds.ToString()
    Users           = $users
}
$probe | Format-List | Out-String | Tee-Object -FilePath (Join-Path $OutDir 'session.txt') | Write-Host

# --- Install --------------------------------------------------------------------
if ($Installer) {
    $AppDir = Join-Path $env:RUNNER_TEMP 'TGZTerminal-smoke-install'
    Write-Step "Installing $Installer into $AppDir"
    $setup = Start-Process -FilePath $Installer -Wait -PassThru -ArgumentList @(
        '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER',
        "/DIR=`"$AppDir`"", "/LOG=`"$(Join-Path $OutDir 'inno.log')`""
    )
    if ($setup.ExitCode -ne 0) { throw "installer exited with $($setup.ExitCode)" }
}
if (-not $AppDir) { throw 'pass -Installer or -AppDir' }
$gui = Join-Path $AppDir 'wezterm-gui.exe'
if (-not (Test-Path $gui)) { throw "no wezterm-gui.exe in $AppDir" }

# --- Launch ---------------------------------------------------------------------
$logDir = Join-Path $env:USERPROFILE '.local\share\wezterm'
Get-ChildItem $logDir -Filter '*-log-*.txt' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

$consoleClasses = @('ConsoleWindowClass', 'CASCADIA_HOSTING_WINDOW_CLASS')
$consolesBefore = @([TgzWin]::Visible() | Where-Object { $consoleClasses -contains $_.Class } | ForEach-Object { $_.Handle })

$env:WEZTERM_LOG = 'info,window=debug'
if ($Config) { $env:WEZTERM_CONFIG_FILE = (Resolve-Path $Config).Path }
Write-Step "Launching $gui"
$proc = Start-Process -FilePath $gui -ArgumentList @('start', '--always-new-process') -PassThru

$flashes = @{}
function Watch-Consoles {
    foreach ($w in [TgzWin]::Visible()) {
        if ($consoleClasses -contains $w.Class -and $consolesBefore -notcontains $w.Handle) {
            $flashes[$w.Handle] = $w.ToString()
        }
    }
}

$window = $null
$dialog = $null
$deadline = (Get-Date).AddSeconds($WindowTimeout)
while ((Get-Date) -lt $deadline -and -not $proc.HasExited) {
    Watch-Consoles
    $own = @([TgzWin]::Visible() | Where-Object { $_.Pid -eq $proc.Id })
    $dialog = $own | Where-Object { $_.Class -eq '#32770' } | Select-Object -First 1
    if ($dialog) { break }
    $window = $own | Where-Object { $_.Class -ne '#32770' } | Select-Object -First 1
    if ($window) { break }
    Start-Sleep -Milliseconds 100
}

if ($dialog) {
    $failures.Add("error dialog: $([TgzWin]::AllText($dialog.Handle))")
} elseif (-not $window) {
    $failures.Add($(if ($proc.HasExited) { "exited with code $($proc.ExitCode) before showing a window" } else { "no window within $WindowTimeout s" }))
} else {
    Write-Step "Window up: $window"
    $stayUntil = (Get-Date).AddSeconds($StaySeconds)
    $expectUntil = $stayUntil.AddSeconds($ExpectTimeout)
    while (-not $proc.HasExited) {
        $now = Get-Date
        if ($now -ge $expectUntil) { break }
        if ($now -ge $stayUntil) {
            $current = (Get-ChildItem $logDir -Filter '*-log-*.txt' -ErrorAction SilentlyContinue |
                ForEach-Object { Get-Content $_.FullName -Raw -ErrorAction SilentlyContinue }) -join "`n"
            if (-not ($ExpectLog | Where-Object { $current -notmatch $_ })) { break }
        }
        Watch-Consoles
        $dialog = [TgzWin]::Visible() | Where-Object { $_.Pid -eq $proc.Id -and $_.Class -eq '#32770' } | Select-Object -First 1
        if ($dialog) { $failures.Add("error dialog: $([TgzWin]::AllText($dialog.Handle))"); break }
        Start-Sleep -Milliseconds 100
    }
    if ($proc.HasExited) { $failures.Add("exited with code $($proc.ExitCode) $StaySeconds s after showing its window") }
}
foreach ($flash in $flashes.Values) { $failures.Add("console window appeared: $flash") }

# --- Evidence -------------------------------------------------------------------
try {
    $bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
    [System.Drawing.Graphics]::FromImage($bitmap).CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
    $bitmap.Save((Join-Path $OutDir 'desktop.png'))
} catch { Write-Warning "screenshot failed: $_" }

if (-not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
Start-Sleep -Seconds 1

$log = ''
foreach ($file in Get-ChildItem $logDir -Filter '*-log-*.txt' -ErrorAction SilentlyContinue) {
    Copy-Item $file.FullName $OutDir
    $log += Get-Content $file.FullName -Raw
}
if (-not $log) { $failures.Add("no log file in $logDir") }
Write-Host '---- log ----'
Write-Host $log
Write-Host '-------------'

foreach ($bad in @('panic at ', 'caught ', '; terminating', 'Failed to create window')) {
    if ($log.Contains($bad)) { $failures.Add("log contains '$bad'") }
}
foreach ($pattern in $ExpectLog) {
    if ($log -notmatch $pattern) { $failures.Add("log never matched /$pattern/") }
}

if ($failures.Count) {
    Write-Host '::group::Smoke test failures'
    $failures | ForEach-Object { Write-Host "::error::$_" }
    Write-Host '::endgroup::'
    exit 1
}
Write-Step 'Smoke test passed'
# Explicit: the Actions pwsh wrapper exits with $LASTEXITCODE, which the last
# native command (`query user` exits 1 on the runner) left nonzero.
exit 0
