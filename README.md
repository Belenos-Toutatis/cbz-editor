# Éditeur CBZ

Éditeur graphique de fichiers **CBZ** (bandes dessinées / mangas), écrit en Rust avec
[egui](https://github.com/emilk/egui). Pour préparer des CBZ propres destinés à
**Komga / Kavita / YACReader** : nettoyer les pages, gérer les doubles-pages, poser la couverture.

<p align="center"><img src="assets/icon-256.png" width="160" alt="Éditeur CBZ"></p>

## Fonctionnalités

- 🗑️ **Supprimer** des pages · 🔀 **réordonner** (boutons, ou **glisser** une vignette à sa place avec repère) · ➕ **insérer** une image (clic droit, ou **glisser-déposer** sur la grille).
- 🪓 **Découper** une double-page en deux (ligne de coupe **déplaçable**, sens **RTL/LTR**) · 🔗 **fusionner** deux pages en double-page.
- ✂️ **Recadrer** une page en **couverture** (`000_cover.jpg`), cadre déplaçable/redimensionnable.
- 🔍 **Agrandir** une page (flèches pour feuilleter, Échap pour revenir) · 💾 **extraire** une page vers un fichier.
- Conserve le `ComicInfo.xml` et **recalcule le `PageCount`** ; écriture en ZIP *stored*.
- Enregistrement **sûr** : copie « … (édité).cbz » ou écrasement avec sauvegarde `.bak` automatique.

## Installation

### Debian / Ubuntu
Récupérez le `.deb` dans les [Releases](../../releases), puis :
```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portable, toutes distributions)
```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Utilisation
```bash
cbz-editor                 # puis « Ouvrir un CBZ… » ou glisser-déposer un .cbz
cbz-editor "Tome.cbz"      # ouvre directement un fichier
```
**Clic droit** sur une page pour tout le menu (découper, fusionner, insérer, extraire, déplacer).
**Glisser** une vignette pour la déplacer ; **glisser** une image depuis l'explorateur pour l'insérer.

## Compiler / empaqueter
```bash
cargo run --release          # lancer
cargo build --release        # binaire → target/release/cbz-editor
cargo deb                    # paquet .deb (cargo install cargo-deb)
```
Pour l'AppImage et le détail du dev, voir [CLAUDE.md](CLAUDE.md).

Dépendances au runtime : **OpenGL** (`libgl1`/`libegl1`) et **`xdg-desktop-portal`** (dialogue de
fichiers natif). Le glisser-déposer s'appuie sur **X11/XWayland**.

## Licence
[MIT](LICENSE).
