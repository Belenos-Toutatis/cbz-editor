# CBZ-Editor

**[Français](README.md) · [English](README.en.md) · [Español](README.es.md) · Deutsch · [Italiano](README.it.md)**

Ein grafischer Editor für **CBZ**-Dateien (Comics / Manga), geschrieben in Rust mit
[egui](https://github.com/emilk/egui). Zum Vorbereiten sauberer CBZ-Dateien für
**Komga / Kavita / YACReader**: Seiten aufräumen, Doppelseiten verwalten, Cover festlegen.

<p align="center"><img src="assets/icon-256.png" width="160" alt="CBZ-Editor"></p>

## Funktionen

- 🗑️ Seiten **löschen** · 🔀 **umordnen** (Schaltflächen oder ein Vorschaubild mit Markierung an seinen Platz **ziehen**) · ➕ ein Bild **einfügen** (Rechtsklick oder **Drag-and-drop** auf das Raster).
- 🪓 Eine Doppelseite in zwei **teilen** (**verschiebbare** Schnittlinie, **RTL/LTR**-Richtung) · 🔗 zwei Seiten zu einer Doppelseite **zusammenführen**.
- 🧹 **Duplikate markieren**: markiert Seiten mit **identischem Inhalt** (Bytes) mit einem Klick zum Löschen.
- ✂️ Eine Seite als **Cover** **zuschneiden** (`000_cover.jpg`), verschiebbarer/größenveränderbarer Rahmen.
- 🔍 Eine Seite **vergrößern** (Pfeiltasten zum Blättern, **Klick = Originalgröße**, Esc zum Zurück) · 💾 eine Seite in eine Datei **extrahieren**.
- 🌍 **Oberfläche in 5 Sprachen** — Französisch, Englisch, Spanisch, Deutsch, Italienisch (anhand der Locale erkannt, in der oberen Leiste umschaltbar).
- Gelesene Formate: **JPG, PNG, WebP, GIF, BMP**; Seiten in **natürlicher Reihenfolge** sortiert (`page2` vor `page10`).
- Behält `ComicInfo.xml` und **berechnet `PageCount` neu** (der `<Pages>`-Block wird entfernt, wenn er nicht mehr passt); als *stored* ZIP geschrieben. Das Cover steht immer **zuerst**.
- **Sicheres** Speichern: eine Kopie „… (bearbeitet).cbz“ (wird nie überschrieben) oder Überschreiben des Originals mit automatischem `.bak`-Backup.
- ↩️ **Rückgängig / Wiederholen** für alle Änderungen · Warnung vor dem **Schließen** oder **Öffnen** einer anderen Datei bei **nicht gespeicherten Änderungen**.

## Installation

### Debian / Ubuntu
Hol dir die `.deb` aus den [Releases](../../releases), dann:
```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portabel, alle Distributionen)
```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Verwendung
```bash
cbz-editor                 # dann „CBZ öffnen…“ oder eine .cbz per Drag-and-drop
cbz-editor "Band.cbz"      # eine Datei direkt öffnen
```
**Rechtsklick** auf eine Seite für das ganze Menü (teilen, zusammenführen, einfügen, extrahieren, verschieben).
Ein Vorschaubild zum Verschieben **ziehen**; ein Bild aus dem Dateimanager zum Einfügen **ziehen**.
Die **Sprache** folgt deinem System; ein Auswahlfeld oben rechts ändert sie (wird gemerkt).

### Tastenkürzel
| Wo | Tasten |
|---|---|
| Überall | **Strg+O** öffnen · **Strg+S** Kopie speichern · **Strg+Z** rückgängig · **Strg+Y** / **Strg+Umschalt+Z** wiederholen |
| Vergrößerte Ansicht | **← / →** vorherige/nächste Seite · **Pos1 / Ende** · **Entf** markieren/behalten · **Esc** schließen |
| Teilen | **← / →** Schnitt verschieben (**Umschalt** = größere Schritte) · **Eingabe** übernehmen · **Esc** abbrechen |
| Zuschneiden | **Eingabe** übernehmen · **Esc** abbrechen |

## Bauen / paketieren
```bash
cargo run --release          # starten
cargo build --release        # Binärdatei → target/release/cbz-editor
cargo deb                    # .deb-Paket (cargo install cargo-deb)
```
Für die AppImage und Entwicklungsdetails siehe [CLAUDE.md](CLAUDE.md).

Laufzeitabhängigkeiten: **OpenGL** (`libgl1`/`libegl1`) und **`xdg-desktop-portal`** (nativer
Dateidialog). Drag-and-drop nutzt **X11/XWayland**.

## Lizenz
[MIT](LICENSE).
