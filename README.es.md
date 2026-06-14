# Editor CBZ

**[Français](README.md) · [English](README.en.md) · Español · [Deutsch](README.de.md) · [Italiano](README.it.md)**

Editor gráfico de archivos **CBZ** (cómics / manga), escrito en Rust con
[egui](https://github.com/emilk/egui). Para preparar archivos CBZ limpios para
**Komga / Kavita / YACReader**: limpiar páginas, gestionar páginas dobles, poner la portada.

<p align="center"><img src="assets/icon-256.png" width="160" alt="Editor CBZ"></p>

## Funciones

- 🗑️ **Eliminar** páginas · 🔀 **reordenar** (botones, o **arrastrar** una miniatura a su sitio con un marcador) · ➕ **insertar** una imagen (clic derecho, o **arrastrar y soltar** en la cuadrícula).
- 🪓 **Dividir** una página doble en dos (línea de corte **desplazable**, sentido **RTL/LTR**) · 🔗 **fusionar** dos páginas en una página doble.
- 🧹 **Marcar duplicados**: marca para eliminar las páginas con **contenido idéntico** (bytes) en un clic.
- ✂️ **Recortar** una página como **portada** (`000_cover.jpg`), marco desplazable/redimensionable.
- 🔍 **Ampliar** una página (flechas para pasar páginas, Esc para volver) · 💾 **extraer** una página a un archivo.
- 🌍 **Interfaz en 5 idiomas** — francés, inglés, español, alemán, italiano (detectado según la configuración regional, cambiable en la barra superior).
- Formatos leídos: **JPG, PNG, WebP, GIF, BMP**; páginas ordenadas en **orden natural** (`page2` antes que `page10`).
- Conserva `ComicInfo.xml` y **recalcula `PageCount`**; escritura como ZIP *stored*.
- Guardado **seguro**: una copia «… (editado).cbz» (nunca sobrescrita) o sobrescribir el original con copia `.bak` automática.

## Instalación

### Debian / Ubuntu
Obtén el `.deb` desde las [Releases](../../releases), luego:
```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portable, todas las distribuciones)
```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Uso
```bash
cbz-editor                 # luego «Abrir un CBZ…» o arrastrar y soltar un .cbz
cbz-editor "Tomo.cbz"      # abre un archivo directamente
```
**Clic derecho** en una página para el menú completo (dividir, fusionar, insertar, extraer, mover).
**Arrastra** una miniatura para moverla; **arrastra** una imagen desde el explorador para insertarla.
El **idioma** sigue tu sistema; un selector arriba a la derecha permite cambiarlo (se recuerda).

## Compilar / empaquetar
```bash
cargo run --release          # ejecutar
cargo build --release        # binario → target/release/cbz-editor
cargo deb                    # paquete .deb (cargo install cargo-deb)
```
Para la AppImage y los detalles de desarrollo, consulta [CLAUDE.md](CLAUDE.md).

Dependencias en tiempo de ejecución: **OpenGL** (`libgl1`/`libegl1`) y **`xdg-desktop-portal`** (diálogo
de archivos nativo). El arrastrar y soltar usa **X11/XWayland**.

## Licencia
[MIT](LICENSE).
