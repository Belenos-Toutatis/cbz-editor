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
- **i18n** : toutes les chaînes d'UI passent par `tr(lang, T::Clé)` — table `Lang` × `T` indexée
  par `lang as usize` (5 colonnes : FR, EN, ES, DE, IT, **dans cet ordre**). `Lang::detect()` lit
  la locale (`LC_ALL`/`LC_MESSAGES`/`LANG`, repli EN) ; choix mémorisé via `save_lang`/`load_lang`
  (`~/.config/cbz-editor/lang`) ; sélecteur (ComboBox) dans la barre du haut. Les marqueurs
  `{n}` `{e}` `{name}`… sont remplacés à l'usage (`.replace`). **Ajouter une chaîne** = une variante
  `T` + une ligne dans `tr` avec **5** entrées. Pièges d'emprunt : les méthodes UI captent
  `let lang = self.lang;` en tête (car `self.split/crop/viewer` ou `self.pages.iter_mut()` sont
  empruntés pendant l'affichage → on ne peut pas rappeler `&self`).

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
- `last_dir` et la langue persistés dans `~/.config/cbz-editor/` (`last_dir`, `lang`).
- egui : closures imbriquées qui écrivent une variable locale (`action`, `dragged_uid`…) — ok
  car appelées séquentiellement ; capture disjointe des champs de `self`.

## Publier une version
Le `.deb` et l'AppImage sont construits et **attachés à la Release automatiquement** par
GitHub Actions (`.github/workflows/release.yml`) au **push d'un tag `vX.Y.Z`**.
1. Bumper `version` dans `Cargo.toml` ; `git commit` + `git push`.
2. `git tag vX.Y.Z && git push origin vX.Y.Z` → la CI compile et publie la Release
   (notes auto-générées, éditables ensuite ; pour la rendre brouillon, ajouter `draft: true`
   au step `softprops/action-gh-release`).

Détails CI : runner Ubuntu, build `--locked`, `.deb` via `cargo-deb` (`--no-build`), AppImage via
linuxdeploy/appimagetool **téléchargés** (le `.aitools/` local n'est pas versionné), nom de sortie
forcé par `OUTPUT=cbz-editor-x86_64.AppImage`, `APPIMAGE_EXTRACT_AND_RUN=1` (pas de FUSE).
Les **binaires ne sont jamais commités** (cf. `.gitignore`), seulement attachés à la Release.

Build local (essais) : `cargo deb` + la commande AppImage ci-dessus (outils dans `.aitools/`).

**Confidentialité** : l'e-mail des commits est l'e-mail *noreply* GitHub, fixé en **config locale
au dépôt** (`git config user.email` local). Ne pas remettre d'e-mail personnel dans les fichiers
versionnés ni dans `Cargo.toml` (`maintainer`).

## Conventions
- **Commentaires** et noms de symboles en **français** ; **interface multilingue** (FR/EN/ES/DE/IT)
  via la table `tr` (voir « i18n » plus haut).
- **README par langue** : `README.md` (FR, primaire) + `README.en.md` / `README.es.md` /
  `README.de.md` / `README.it.md`. Garder la **ligne de navigation** en tête de chaque fichier
  synchronisée, et refléter tout changement de fonctionnalités dans les 5.
- Fichiers `.desktop` (`assets/`) : champs `Name[xx]` / `GenericName[xx]` / `Comment[xx]` localisés.
  Le **défaut (non suffixé) reste en français** — le nom de sortie de l'AppImage en dépend.
- `.gitignore` exclut `target/`, `AppDir/`, `.aitools/`, `*.AppImage`, `*.deb`.
