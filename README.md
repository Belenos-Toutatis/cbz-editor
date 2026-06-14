# Éditeur CBZ

Un petit éditeur de fichiers **CBZ** (bandes dessinées / mangas) avec interface graphique,
écrit en Rust (egui). Pensé pour nettoyer et préparer des CBZ destinés à Komga / Kavita / YACReader.

![icône](assets/icon-256.png)

## Fonctionnalités

- 🗑️ **Supprimer** des pages, 🔀 **réordonner** (boutons, ou **glisser une vignette** à sa nouvelle place
  avec repère visuel), ➕ **insérer** une image (clic droit, ou **glisser-déposer** une image sur la grille).
- 🪓 **Découper** une double-page en deux pages (ligne de coupe **déplaçable**, sens de lecture RTL/LTR).
- 🔗 **Fusionner** deux pages en une double-page (sens de lecture respecté).
- ✂️ **Recadrer** une page pour en faire la **couverture** (`000_cover.jpg`), avec cadre déplaçable/redimensionnable.
- 🔍 **Agrandir** une page (flèches pour feuilleter, Échap pour revenir).
- 💾 **Extraire** une page vers un fichier (utile quand la couverture du tome suivant est rangée dans le mauvais tome).
- Conserve le `ComicInfo.xml`, recalcule le `PageCount`, écrit en ZIP *stored*.
- Enregistrement **sûr** : copie `… (édité).cbz` ou écrasement avec sauvegarde `.bak` automatique.

## Utilisation

```bash
cbz-editor                       # puis « Ouvrir un CBZ… » ou glisser-déposer
cbz-editor "Mon Tome.cbz"        # ouvre directement un fichier
```

Clic droit sur une page pour le menu complet (découper, fusionner, insérer, extraire, déplacer).

## Installation

### Paquet Debian/Ubuntu (.deb)

```bash
sudo apt install ./cbz-editor_*.deb
```

### AppImage (portable, toutes distributions)

```bash
chmod +x cbz-editor-x86_64.AppImage
./cbz-editor-x86_64.AppImage
```

## Compiler depuis les sources

```bash
cargo build --release
# paquet .deb :   cargo deb
```

Dépendances système au runtime : OpenGL (`libgl1`/`libegl1`) et `xdg-desktop-portal`
(pour le dialogue de fichiers natif). Le glisser-déposer utilise le backend X11/XWayland.

## Licence

MIT — voir [LICENSE](LICENSE).
