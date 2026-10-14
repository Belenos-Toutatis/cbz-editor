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
cargo test --release           # tests de la logique hors GUI (module `tests` en fin de main.rs)
cargo clippy --release         # doit rester sans avertissement
```
**AppImage** (outils dans `.aitools/`, non versionnés) :
```bash
export APPIMAGE_EXTRACT_AND_RUN=1
./.aitools/linuxdeploy --appdir AppDir -e target/release/cbz-editor \
  -d assets/cbz-editor-appimage.desktop -i assets/icon-256.png \
  --icon-filename cbz-editor --output appimage
mv "Éditeur_CBZ-x86_64.AppImage" cbz-editor-x86_64.AppImage
```
Tests automatisés **uniquement pour la logique** (`build_cbz`, ComicInfo, tri naturel, Défaire,
lecture ZIP…). L'interface se vérifie en lançant l'appli avec un vrai `.cbz` — **toujours une copie**.
Pour piloter la GUI (xdotool + captures), utiliser un **Xvfb** (`Xvfb :77` puis
`env -u WAYLAND_DISPLAY DISPLAY=:77 …`) : sous la session Wayland, les clics simulés n'atteignent
pas la fenêtre XWayland. Dans Xvfb : `xdotool windowfocus` avant les touches, et des clics en
`mousedown`/`mouseup` séparés (~100 ms) sinon egui ne voit pas le clic.

## Architecture (`src/main.rs`)
- `CbzApp` : état global. Pages en mémoire (`Vec<Page>`, octets d'origine conservés).
- Modes mutuellement exclusifs, testés dans cet ordre dans `update()` :
  `viewer` (agrandissement) → `split` (découpe) → `crop` (recadrage couverture) → grille.
- `Page { name, bytes: Arc<[u8]>, thumb, failed, delete, uid }` (`Clone` bon marché : octets
  partagés). Le `uid` (stable) sert d'identité pour les miniatures, le glisser-réordonner et la
  resynchronisation après écrasement (pas l'index, qui bouge). Nouveaux uid via `new_uid()`.
- **Rien de lourd sur le thread de l'UI** : ouverture d'un .cbz (`open_cbz` lance `read_cbz`
  via `spawn_job`, `poll_opening` applique le résultat ; indicateur de progression, UI en lecture
  seule pendant ce temps) et page agrandie (`ViewerState::pump` décode la page courante puis la
  suivante/précédente, cache limité à ces 3 pages par uid). Seuls restent synchrones les
  traitements ponctuels déclenchés par un clic (entrée en recadrage/découpe, fusion, enregistrement).
- Miniatures décodées **en arrière-plan** (`ThumbWorkers` : 1-4 threads, canaux mpsc) ; l'UI ne fait
  que téléverser la texture. `generation` (incrémentée à l'ouverture) invalide les travaux d'un
  fichier précédent ; `thumb_pending` évite les doublons d'envoi.
- **Défaire/Refaire** : `checkpoint()` à appeler **juste avant chaque modification** (et seulement
  si elle a lieu) ; pile de `Snapshot { pages, cover, dirty_order }` (max `UNDO_MAX`). Historique
  vidé à l'ouverture et après « Écraser l'original ».
- **Modifications non enregistrées** : `edit_sig()` (empreinte uid+delete+couverture) comparée à
  `saved_sig` → `is_modified()`. Garde-fou (`confirm_discard`/`Pending`) avant ouvrir/déposer un
  autre .cbz et à la fermeture (`close_requested` + `CancelClose`). Ouvrir passe par `request_open`.
- **Barre d'état** en bas : `info(msg)` / `fail(msg)` (rouge) — ne pas affecter `status` directement.
- **Palette** : couleurs centralisées dans `pal(ctx)` (variantes thème sombre/clair, egui suit le
  système). Pas de `Color32` en dur dans l'UI, sauf superpositions dessinées sur une image.
- Boîtes de confirmation non modales en egui 0.29 → le reste de l'UI est `ui.disable()` tant
  qu'une boîte est ouverte (`modal` dans `update`), raccourcis désactivés, Échap = annuler.
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
- **Glyphes** : les polices d'egui (Ubuntu-Light + NotoEmoji + emoji-icon-font ; Hack n'est que
  pour la police **mono**) n'ont pas tout : `→ ← ↶ ↷ ⇄ ● ◫ 🧹 🪓 ⤒ ⤓ ʳ ᵉ` s'affichent en carré
  vide. Vérifier tout nouveau symbole avec fontTools sur `epaint_default_fonts-*/fonts/` (sans Hack).
  Choix actuels : ⟲/⟳ défaire/refaire, ⬅/➡ flèches, ✂ recadrer, 📖 découper, 🗐 doublons, ⏮/⏭.
- **Enregistrement** (`build_cbz` → `Built`) : couverture en `000_cover.<ext>` (une **seule** page
  `000_cover` existante sert de couverture, avec son extension) ; `ComicInfo.xml` conservé,
  `PageCount` réécrit (au **niveau octets**, pour préserver un XML non‑UTF‑8). Les pages gardent
  leur **nom d'origine** SAUF si l'ordre a changé (`dirty_order` : réordre/insertion/fusion/découpe)
  → renumérotées `p0000.jpg`… pour figer l'ordre (les lecteurs trient par nom) — renumérotation
  aussi si une page serait triée avant la couverture. Bloc `<Pages>` du ComicInfo retiré
  (`strip_pages_block`) dès que la liste change. Refus d'enregistrer si aucune page ne reste.
  Après « Écraser », `sync_after_overwrite` aligne l'état mémoire sur le fichier écrit (pages
  supprimées retirées, noms définitifs, couverture devenue page 1). Écritures avec `fsync`. Écriture
  **atomique** (.tmp puis rename) + `.bak`. « Enregistrer une copie » n'écrase jamais une copie
  existante (suffixe `(édité 2)`…). Fusion/découpe/recadrage passent par `encode_page` : **PNG** si
  toutes les sources sont sans perte (`is_lossless` : PNG/BMP/GIF), sinon JPEG q92 (`encode_jpeg`,
  qui remonte ses erreurs et aplatit la transparence sur blanc). L'extension suit (`Cover.ext`). Lecture : entrées `__MACOSX/` et `._*` ignorées.
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
- Code formaté avec `rustfmt` (la table `tr`, trop large, est laissée telle quelle par rustfmt :
  garder **une ligne par clé**).
- **Commentaires** et noms de symboles en **français** ; **interface multilingue** (FR/EN/ES/DE/IT)
  via la table `tr` (voir « i18n » plus haut).
- **README par langue** : `README.md` (FR, primaire) + `README.en.md` / `README.es.md` /
  `README.de.md` / `README.it.md`. Garder la **ligne de navigation** en tête de chaque fichier
  synchronisée, et refléter tout changement de fonctionnalités dans les 5.
- Fichiers `.desktop` (`assets/`) : champs `Name[xx]` / `GenericName[xx]` / `Comment[xx]` localisés.
  Le **défaut (non suffixé) reste en français** — le nom de sortie de l'AppImage en dépend.
- `.gitignore` exclut `target/`, `AppDir/`, `.aitools/`, `*.AppImage`, `*.deb`.
