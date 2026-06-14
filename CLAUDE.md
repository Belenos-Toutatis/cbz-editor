# Éditeur CBZ — notes projet

Application **mono-fichier** en **Rust + egui/eframe** : un éditeur graphique de fichiers CBZ
(ZIP d'images + `ComicInfo.xml`). Projet **autonome** (rien à voir avec une bibliothèque de
mangas en particulier). Tout le code tient dans `src/main.rs`.

## Commandes
```bash
cargo run --release            # lancer (passer un .cbz en argument pour l'ouvrir)
cargo build --release          # binaire -> target/release/cbz-editor
cargo build                    # debug : compile vite, mais décodage d'images lent
cargo deb                      # paquet .deb (nécessite cargo-deb)
```
**AppImage** (outils dans `.aitools/`, non versionnés) :
```bash
export APPIMAGE_EXTRACT_AND_RUN=1
./.aitools/linuxdeploy --appdir AppDir -e target/release/cbz-editor \
  -d assets/cbz-editor-appimage.desktop -i assets/icon-256.png \
  --icon-filename cbz-editor --output appimage
mv "Éditeur_CBZ-x86_64.AppImage" cbz-editor-x86_64.AppImage
```
Pas de tests automatisés : la logique est surtout interactive (GUI). Vérifier en lançant
l'appli avec un vrai `.cbz` (un « smoke test » de 3-4 s suffit à confirmer qu'elle démarre).

## Architecture (`src/main.rs`)
- `CbzApp` : état global. Pages en mémoire (`Vec<Page>`, octets d'origine conservés).
- Modes mutuellement exclusifs, testés dans cet ordre dans `update()` :
  `viewer` (agrandissement) → `split` (découpe) → `crop` (recadrage couverture) → grille.
- `Page { name, bytes, thumb, delete, uid }`. Le `uid` (stable) sert d'identité pour les
  miniatures et le glisser-réordonner (pas l'index, qui bouge).
- Miniatures générées **paresseusement** (quota par frame) pour ne pas figer l'UI.

## Pièges / décisions importantes
- **Dialogue de fichiers** : `rfd` en backend **`xdg-portal` + `async-std`** (PAS `gtk3`).
  Choisi pour des paquets légers (aucune dépendance GTK) ; le portail xdg fournit le dialogue
  natif via D-Bus. ⚠️ La feature `tokio` de rfd **panique** en API sync (« no reactor ») ;
  `async-std` marche.
- **X11 forcé** : `main()` retire `WAYLAND_DISPLAY` pour que winit utilise X11/XWayland, où le
  **glisser-déposer de fichiers** fonctionne (le Wayland natif de winit ne le livre pas).
- **Drop de fichier externe** : la **position** n'est pas fiable sous X11 (winit ne transmet pas
  la position du curseur pendant un drag externe → `hovered_files` vide). Du coup une image lâchée
  est **toujours insérée en page 1** (en tête) et un **toast** (~`TOAST_SECS` s, champ `toast`) le
  signale. Le **glisser interne** d'une vignette, lui, est précis (egui suit le pointeur) →
  c'est le moyen fiable de **repositionner** une page ensuite.
- **Doublons** : « 🧹 Marquer les doublons » (barre du haut, `mark_all_duplicates`) et « Cocher
  les copies identiques » (clic droit, `mark_identical`) cochent pour suppression les pages au
  **contenu strictement identique** (octets, via `duplicate_groups`/`content_hash`). **Toutes** les
  copies sont cochées, on n'en garde aucune. Comparaison **exacte** (pas de similarité perceptuelle)
  → sûr, mais ne repère pas un même visuel ré-encodé.
- **Lecture robuste** : pages triées en **ordre naturel** (`natural_cmp` : `p2` < `p10`, jamais
  de tri lexical) ; lecture ZIP **plafonnée** (512 Mio/entrée, 4 Gio au total) contre les bombes
  zip ; formats décodés **JPEG/PNG/WebP/GIF/BMP** ; une page illisible affiche un cadre
  « ⚠ illisible » cliquable (jamais de spinner infini, ni en grille ni dans le viewer).
- **Enregistrement** (`build_cbz`) : couverture en `000_cover.jpg` ; `ComicInfo.xml` conservé,
  `PageCount` réécrit (au **niveau octets**, pour préserver un XML non‑UTF‑8). Les pages gardent
  leur **nom d'origine** SAUF si l'ordre a changé (`dirty_order` : réordre/insertion/fusion/découpe)
  → renumérotées `p0000.jpg`… pour figer l'ordre (les lecteurs trient par nom). Écriture
  **atomique** (.tmp puis rename) + `.bak`. « Enregistrer une copie » n'écrase jamais une copie
  existante (suffixe `(édité 2)`…). `encode_jpeg` remonte ses erreurs (pas de page vide silencieuse).
- **Sens de lecture** (`rtl`) déduit du `ComicInfo` (`YesAndRightToLeft`), modifiable dans l'UI ;
  il pilote l'ordre des fusion/découpe.
- `last_dir` persisté dans `~/.config/cbz-editor/last_dir`.
- egui : closures imbriquées qui écrivent une variable locale (`action`, `dragged_uid`…) — ok
  car appelées séquentiellement ; capture disjointe des champs de `self`.

## Publier une version
1. Bumper `version` dans `Cargo.toml`.
2. `cargo build --release` ; `cargo deb` ; reconstruire l'AppImage (ci-dessus).
3. `git commit` + `git push`.
4. `gh release create vX.Y.Z --title … --notes … <.deb> <.AppImage>`.

**Confidentialité** : l'e-mail des commits est l'e-mail *noreply* GitHub, fixé en **config locale
au dépôt** (`git config user.email` local). Ne pas remettre d'e-mail personnel dans les fichiers
versionnés ni dans `Cargo.toml` (`maintainer`).

## Conventions
- Interface et commentaires en **français**.
- `.gitignore` exclut `target/`, `AppDir/`, `.aitools/`, `*.AppImage`, `*.deb`.
