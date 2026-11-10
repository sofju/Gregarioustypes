# Gregarioustypes

*File types are gregarious creatures: they never travel alone, they gather by
the hundred in every Downloads folder, and each one quietly longs to be looked
at, so Gregarioustypes greets all two hundred of them by name the moment you
press Space.*

Select a file in Explorer, on the desktop or in an Open/Save dialog, press
<kbd>Space</kbd>, and a preview floats up at once, in the spirit of macOS Quick
Look. Press <kbd>Space</kbd> again and it's gone.

- **Fast.** The previewer stays warm in the background, so the window appears
  almost instantly and the file follows.
- **Everything in one place.** Images (HDR included), video and audio, PDF and
  Office documents, design files, archives and disc images, 3D models, code,
  torrents and scene NFOs.
- **Self-contained.** Nothing else to install: every engine it needs comes with it.

## Install

1. Download `Gregarioustypes-0.1.0-setup.exe` from the
   [latest release](https://github.com/sofju/Gregarioustypes/releases/latest).
2. Run it. It installs for your account only, without administrator rights.
   - Windows may show *"Windows protected your PC"*, because the installer isn't
     signed: click **More info**, then **Run anyway**.
   - **Full** installs everything (about 3 GB on disk). **Compact** leaves out
     LibreOffice and Blender (about 350 MB): Word, PowerPoint and `.blend` files
     then show only the preview saved inside them.
3. Done: a tray icon appears, and <kbd>Space</kbd> now previews files.

Needs 64-bit Windows 10 (version 2004 or later) or Windows 11. To remove it, use
**Settings › Apps › Installed apps › Gregarioustypes › Uninstall**.

## Use

| Key | What it does |
|---|---|
| <kbd>Space</kbd> | Preview the selected file; press again (or <kbd>Esc</kbd>) to close |
| <kbd>←</kbd> <kbd>→</kbd> <kbd>↑</kbd> <kbd>↓</kbd> | Move through the folder (or through the selected files); the preview follows |
| <kbd>Alt</kbd>+<kbd>Space</kbd>, <kbd>F11</kbd> | Full screen |
| <kbd>Enter</kbd> | Open the file in its usual app |
| <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Index sheet: all the selected files as thumbnails |
| <kbd>Ctrl</kbd>+<kbd>I</kbd> | File information |
| <kbd>Ctrl</kbd>+<kbd>M</kbd> | MediaInfo report (media files, discs) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> | Copy the text shown |
| <kbd>Backspace</kbd> | Back out of a file opened from inside an archive |

The buttons in the preview's title bar open the file, share it, rotate images,
pin the window and more. Double-click the tray icon (or use the Start menu's
*Gregarioustypes settings*) to choose engines per format, autoplay, cache size and
display options.

## What it previews

| Family | Formats (and highlights) |
|---|---|
| Images | JPEG (with Ultra HDR gain maps), PNG/APNG, GIF, WebP, AVIF, HEIC/HEIF, JPEG XL, JPEG XR, JPEG 2000, TIFF, EXR, Radiance HDR, DNG, PSD/PSB, BMP, ICO, ICNS, TGA, SVG. HDR images are shown in HDR on HDR displays and tone-mapped elsewhere |
| Video and audio | MKV, MP4, MOV, AVI, TS/M2TS, WMV, FLV, VOB, RealMedia… and MP3, FLAC, AAC, Opus, Ogg, WAV, APE, WavPack, DTS…, with HDR10, HLG and Dolby Vision; MIDI files play through a SoundFont; playlists (M3U, PLS, ASX, RAM…) |
| Documents | PDF, Illustrator, EPS/PostScript, RTF, Excel (as a grid), Word, PowerPoint, OpenDocument, HTML, Markdown, Flash (SWF) |
| Design | Photoshop, InDesign (INDD and IDML), Xara, Affinity |
| Archives and discs | ZIP, 7z, RAR, TAR and its compressed forms, DEB, RPM, PKG, DMG, CAB, WIM, ISO, BIN/CUE, MDF, NRG, CDI and more; files inside open as previews too. Blu-ray images get a disc summary, playback of the main title and a BDInfo report |
| 3D | glTF/GLB, OBJ, STL, FBX, USD/USDZ, Alembic, `.blend` |
| Text and code | any text file, with colouring for 220 languages and line numbers |
| And also | torrents (files, trackers, magnet link), NFO/DIZ/ANSI art in the original DOS font, NZB, folders (size and contents) |

## Build from source

You need:

- [Rust](https://rustup.rs) (stable, MSVC toolchain)
- [Visual Studio 2022 Build Tools](https://visualstudio.microsoft.com/downloads/) with
  the *Desktop development with C++* workload (MSVC and the Windows SDK)
- [vcpkg](https://github.com/microsoft/vcpkg) (a git clone, bootstrapped)
- [Python 3](https://www.python.org), for the licence bundle of the installer
- about 15 GB of free disk space

Then, from a PowerShell prompt in the repository:

```powershell
# 1. C/C++ image libraries (vcpkg manifest mode, versions pinned by vcpkg.json)
C:\path\to\vcpkg\vcpkg.exe install --triplet x64-windows-static

# 2. Bundled engines into deps\ (about 1.1 GB to download; checked against
#    scripts\deps.lock; also builds 7z.dll from 7-Zip's source)
powershell -ExecutionPolicy Bypass -File scripts\fetch-deps.ps1

# 3. The app, into target\release
powershell -ExecutionPolicy Bypass -File scripts\build.ps1
.\target\release\Gregarioustypes.exe

# 4. Optional: the installer, into target\installer
powershell -ExecutionPolicy Bypass -File scripts\package.ps1 -NoBuild
```

Tests: `cargo test --workspace --release`. Tests that need sample files skip
themselves when `samples\` is empty; the `scripts\make-*-samples.py` and
`scripts\fetch-*-samples.py` scripts build a sample corpus.

### How it fits together

- **Agent** (`Gregarioustypes.exe`): a few megabytes in the tray. It watches for
  <kbd>Space</kbd> in Explorer, on the desktop and in file dialogs, reads the
  selection, and tells the viewer what to show.
- **Viewer** (`Gregarioustypes.exe --viewer`): kept warm and hidden. It draws with
  Direct3D 11, Direct2D and DirectComposition, with an FP16 scRGB swap chain for HDR.
- **Providers**: one per family of formats (`crates/gt-fmt-*`), each turning a file
  into a view. Heavy engines (mpv, WebView2, LibreOffice, Blender, Ghostscript)
  start on demand and are released after a few idle minutes.

| Crate | Role |
|---|---|
| `gt-app` | entry point: agent, viewer, settings window |
| `gt-shell` | keyboard hook, Explorer and file-dialog selection, shell icons |
| `gt-render`, `gt-ui` | Direct3D/DirectComposition rendering, HDR, widgets |
| `gt-core`, `gt-ipc` | providers registry, sniffing, settings, caches, pipe protocol |
| `gt-image` | native image codecs (C/C++ shims over the vcpkg libraries) |
| `gt-fmt-*` | images, media, documents, web, design, archives, 3D, misc |
| `gt-dialoghook` | helper that reads the selection of Open/Save dialogs |

## Licence

Gregarioustypes is free software: you can redistribute it and/or modify it under
the terms of the [GNU General Public License](LICENSE), version 3 or (at your
option) any later version. It comes with no warranty.

The bundled components keep their own licences, listed with their source code in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md). This product uses MediaInfo
library, Copyright (c) 2002-2025 MediaArea.net SARL.

Quick Look is a trademark of Apple Inc.; this project isn't affiliated with Apple.
