# Editor CBZ

**[Français](README.md) · [English](README.en.md) · [Español](README.es.md) · [Deutsch](README.de.md) · Italiano**

Editor grafico di file **CBZ** (fumetti / manga), scritto in Rust con
[egui](https://github.com/emilk/egui). Per preparare file CBZ puliti per
**Komga / Kavita / YACReader**: ripulire le pagine, gestire le pagine doppie, impostare la copertina.

<p align="center"><img src="assets/icon-256.png" width="160" alt="Editor CBZ"></p>

## Funzionalità

- 🗑️ **Eliminare** pagine · 🔀 **riordinare** (pulsanti, o **trascinare** una miniatura al suo posto con un indicatore) · ➕ **inserire** un'immagine (clic destro, o **trascina e rilascia** sulla griglia).
- 🪓 **Dividere** una pagina doppia in due (linea di taglio **spostabile**, senso **RTL/LTR**) · 🔗 **unire** due pagine in una pagina doppia.
- 🧹 **Segnare i doppioni**: seleziona per l'eliminazione le pagine con **contenuto identico** (byte) in un clic.
- ✂️ **Ritagliare** una pagina come **copertina** (`000_cover.jpg`), riquadro spostabile/ridimensionabile.
- 🔍 **Ingrandire** una pagina (frecce per sfogliare, Esc per tornare) · 💾 **estrarre** una pagina in un file.
- 🌍 **Interfaccia in 5 lingue** — francese, inglese, spagnolo, tedesco, italiano (rilevata dalla locale, modificabile nella barra in alto).
- Formati letti: **JPG, PNG, WebP, GIF, BMP**; pagine ordinate in **ordine naturale** (`page2` prima di `page10`).
- Conserva `ComicInfo.xml` e **ricalcola `PageCount`**; scrittura come ZIP *stored*.
- Salvataggio **sicuro**: una copia «… (modificato).cbz» (mai sovrascritta) o sovrascrittura dell'originale con backup `.bak` automatico.

## Installazione

### Debian / Ubuntu
Scarica il `.deb` dalle [Releases](../../releases), poi:
```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portatile, tutte le distribuzioni)
```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Uso
```bash
cbz-editor                 # poi «Apri un CBZ…» o trascina e rilascia un .cbz
cbz-editor "Volume.cbz"    # apre un file direttamente
```
**Clic destro** su una pagina per il menu completo (dividere, unire, inserire, estrarre, spostare).
**Trascina** una miniatura per spostarla; **trascina** un'immagine dal file manager per inserirla.
La **lingua** segue il sistema; un selettore in alto a destra permette di cambiarla (viene ricordata).

## Compilare / pacchettizzare
```bash
cargo run --release          # avviare
cargo build --release        # binario → target/release/cbz-editor
cargo deb                    # pacchetto .deb (cargo install cargo-deb)
```
Per l'AppImage e i dettagli di sviluppo, vedi [CLAUDE.md](CLAUDE.md).

Dipendenze a runtime: **OpenGL** (`libgl1`/`libegl1`) e **`xdg-desktop-portal`** (finestra di
dialogo dei file nativa). Il trascinamento usa **X11/XWayland**.

## Licenza
[MIT](LICENSE).
