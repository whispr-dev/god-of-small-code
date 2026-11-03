# The Shrine of the God of Small Code

![preview](preview.png)

A small stone brazier that sits on your desktop with an eternal flame in it.
Feed it small files (`.md .txt .py .sh .ps1 .json` ... anything up to 1 MiB) and
the flame consumes them. Pray to it and it answers, cryptically and uselessly.

Think of it as the Recycle Bin's older, stranger cousin: same job (things go in,
your desktop gets tidier), but with fire, and an opinion.

## The one thing to know

**Offerings are moved, not copied.** A file you offer leaves its folder and lands
in the *ashes* folder (`%APPDATA%\GodOfSmallCode\ashes`). Nothing is ever
deleted by an offering, so you can always fetch a file back (right-click the
shrine, **Gather the ashes**). The shrine never opens, reads or runs a file you
give it. It only moves it.

It only accepts plain files up to 1 MiB. Folders and big files make the flame
sputter and a note appears saying why ("the god scorns the large").

## Build (once)

You need Rust 1.80 or newer with the MSVC toolchain (this is the default when
you install Rust from <https://rustup.rs> on Windows; it asks you to install the
Visual Studio Build Tools, tick "Desktop development with C++").

Open a terminal in this folder (in VS Code: **Terminal > New Terminal**) and run:

    cargo build --release

The program is then at `target\release\shrine.exe`. Copy it somewhere permanent,
for example `C:\Tools\shrine\shrine.exe`. It is a single file, about the size of
a large photo, with no installer and nothing to uninstall.

## Use

Double-click `shrine.exe`. The shrine appears bottom-right of your screen.

| To do this | Do this |
|---|---|
| Offer files | Drag them from File Explorer onto the brazier |
| Offer by copy/paste | In Explorer select files, press `Ctrl+C`. Click the shrine once. Press `Ctrl+V` |
| Offer pasted text | Copy any text, click the shrine once, press `Ctrl+V`. It is saved as `pasted-<time>.txt` in the ashes |
| Offer from the right-click menu | Right-click a file in Explorer > **Show more options** > **Send to** > **The God of Small Code** (set up below) |
| Pray | Double-click the shrine (or click it once and press `P`) and type a question. `Enter` prays, `Esc` closes |
| Move the shrine | Drag it. It remembers where you left it |
| Everything else | Right-click the shrine |

Right-click menu: Pray, Offer the clipboard, Gather the ashes (opens the folder),
Scatter the ashes (empties it, asks first), Keep above other windows (on by
default, so drops always reach it), Return to the corner, Banish the shrine (quit).

Running `shrine.exe` a second time never makes a second shrine. It just hands any
files to the one that is already burning (that is how Send To works).

### Add it to the Send To menu (one time)

Paste this into PowerShell, changing the path on the first line to where you put
the exe:

    $exe = "C:\Tools\shrine\shrine.exe"
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\SendTo\The God of Small Code.lnk")
    $s.TargetPath = $exe
    $s.Save()

### Start with Windows (one time)

    $exe = "C:\Tools\shrine\shrine.exe"
    $s = (New-Object -ComObject WScript.Shell).CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Startup\Shrine.lnk")
    $s.TargetPath = $exe
    $s.Save()

To undo either, delete that `.lnk` file.

## Where things live

`%APPDATA%\GodOfSmallCode\` holds `ashes\` (consumed files), `shrine.ini` (position
and the always-on-top setting) and `crash.log` (only exists if the program ever
panics). Paste that path into File Explorer's address bar to open it.

## How light is it?

- Idle: wakes 15 times a second, redraws a 128x160 picture (scaled for your
  screen's DPI), then sleeps. Measured render cost on the build machine
  (Linux, release build): 0.19 ms per frame at 100% scaling, 0.55 ms at 200%,
  which works out to about 0.3% and 0.8% of **one** core. Between frames it does
  nothing at all.
- Burning: 30 frames a second for a couple of seconds, then back to idle.
- Runs at below-normal priority, no tray icon, no taskbar button, no network
  access, no background threads. Memory use was **not** measured on Windows; I
  expect a few MB (the picture buffer is about 330 KB at 200% scaling).
- To make it even sleepier, raise `FRAME_MS_IDLE` in `src/main.rs` (66 means
  about 15 fps; 150 means about 7 fps) and rebuild.

## Project map

    Cargo.toml      pinned: windows-sys =0.61.2 (the only dependency)
    Cargo.lock      commit this
    src/main.rs     Win32 shell: layered window, drag/drop, clipboard, menu, prayer window
    src/gfx.rs      pure-Rust software renderer: brazier, flame, falling offerings, embers
    src/oracle.rs   the oracle (about 100 hand-written omens, combined at random)

`cargo test` runs 13 tests (file moving, name clashes, refusals, config, renderer
at several DPI scales, oracle). Setting the environment variable `SHRINE_DUMP` to a
folder makes the render test write sample frames there as `.ppm` files.

## What was and was not verified

Verified: the whole program type-checks against the real `windows-sys 0.61.2`
function signatures; clippy is clean; all tests pass; I looked at rendered frames
of the idle, burning, prayer-answered and refusal states.

**Not** verified: it has never been run on Windows (the machine this was written on
does not have Windows). If the first launch misbehaves, `crash.log` (above) is the
first place to look. Known limit: Windows blocks drag-and-drop from an
administrator-level window onto a normal-level program, so if you drag from an
elevated app and nothing happens, that is why.

## Under the hood (for the curious)

The shrine is one layered window whose picture is a 32-bit image with per-pixel
transparency, drawn by `UpdateLayeredWindow`. The empty strip above the brazier
is fully click-through; the shrine itself is solid to the mouse but invisible
where it is empty, so drops land even between flame flickers. The flame is a
small grid of heat values (the classic "fire effect") stretched smoothly to the
screen and coloured through a lookup table. Files arrive through the standard
`WM_DROPFILES` message and the clipboard's file-list format.
