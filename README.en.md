# CBZ Editor

**[Français](README.md) · English · [Español](README.es.md) · [Deutsch](README.de.md) · [Italiano](README.it.md)**

A graphical editor for **CBZ** files (comics / manga), written in Rust with
[egui](https://github.com/emilk/egui). For preparing clean CBZ files for
**Komga / Kavita / YACReader**: clean up pages, handle double pages, set the cover.

<p align="center"><img src="assets/icon-256.png" width="160" alt="CBZ Editor"></p>

## Features

- 🗑️ **Delete** pages · 🔀 **reorder** (buttons, or **drag** a thumbnail into place with a marker) · ➕ **insert** an image (right-click, or **drag-and-drop** onto the grid).
- 🪓 **Split** a double page in two (**movable** cut line, **RTL/LTR** direction) · 🔗 **merge** two pages into a double page.
- 🧹 **Mark duplicates**: mark for deletion the pages with **identical content** (bytes) in one click.
- ✂️ **Crop** a page into a **cover** (`000_cover.jpg`), movable/resizable frame.
- 🔍 **Enlarge** a page (arrow keys to flip through, Esc to go back) · 💾 **extract** a page to a file.
- 🌍 **UI in 5 languages** — French, English, Spanish, German, Italian (detected from the locale, switchable from the top bar).
- Formats read: **JPG, PNG, WebP, GIF, BMP**; pages sorted in **natural order** (`page2` before `page10`).
- Keeps `ComicInfo.xml` and **recomputes `PageCount`**; written as a *stored* ZIP.
- **Safe** saving: a “… (edited).cbz” copy (never overwritten) or overwrite the original with an automatic `.bak` backup.

## Installation

### Debian / Ubuntu
Grab the `.deb` from the [Releases](../../releases), then:
```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portable, all distributions)
```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Usage
```bash
cbz-editor                 # then “Open a CBZ…” or drag-and-drop a .cbz
cbz-editor "Volume.cbz"    # open a file directly
```
**Right-click** a page for the full menu (split, merge, insert, extract, move).
**Drag** a thumbnail to move it; **drag** an image from your file manager to insert it.
The **language** follows your system; a selector in the top-right lets you change it (remembered).

## Build / package
```bash
cargo run --release          # run
cargo build --release        # binary → target/release/cbz-editor
cargo deb                    # .deb package (cargo install cargo-deb)
```
For the AppImage and dev details, see [CLAUDE.md](CLAUDE.md).

Runtime dependencies: **OpenGL** (`libgl1`/`libegl1`) and **`xdg-desktop-portal`** (native file
dialog). Drag-and-drop relies on **X11/XWayland**.

## License
[MIT](LICENSE).
