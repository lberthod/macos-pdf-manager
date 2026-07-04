# État précis du projet

**Dernière mise à jour :** 2026-07-04 (Sprint 17+ 6a/6b : édition de texte — `pdf-edit::EditSession` gagne `add_free_text_annotation` (6a, nouveau texte via une annotation `/FreeText` avec apparence réelle) et `replace_text_with_overlay` (6b, "masquer l'ancien + redessiner" : recouvre une zone d'un fond plein puis redessine par-dessus, sans jamais toucher le flux de contenu original) et une méthode générique `remove_annotation` ; 6c (édition chirurgicale du flux existant) volontairement pas engagé, traité comme projet de recherche séparé selon sprint.md. Corrigé au passage : la barre de menus native (Sprint 11-12) pouvait se faire silencieusement écraser par le menu par défaut de `winit` quand lancée via `cargo run` — installée maintenant à la première frame plutôt que dans le callback de création. Voir aussi les mises à jour précédentes : manipulation de pages (Sprint 15-16), premier socle d'édition (Sprint 13-14), chrome natif macOS (Sprint 11-12), harnais de comparaison pixel + corpus élargi à 25 fixtures (Sprint 0/3-4/7-8) — sprint.md pour le détail sprint par sprint)
**But de ce document :** donner une image exacte et vérifiable de ce qui fonctionne, de ce qui est simulé (placeholder), et de ce qui n'existe pas encore — par opposition à [architecture.md](./architecture.md) (la cible) et [sprint.md](./sprint.md) (le plan). Chaque affirmation ci-dessous est vérifiable en lisant le fichier cité ou en lançant la commande indiquée.

---

## 1. Ce qui fonctionne réellement, de bout en bout

Le pipeline suivant fonctionne sur un sous-ensemble réel de PDF (testé sur des fichiers générés par reportlab/pikepdf, pas seulement des fixtures artificiels) :

```
fichier .pdf
  → lexer + parser d'objets COS               (pdf-core/src/lexer.rs, parser.rs)
  → xref classique OU cross-reference stream   (pdf-core/src/xref.rs)
  → objets compressés (object streams)         (pdf-core/src/document.rs)
  → arbre des pages (héritage Resources/MediaBox/Rotate) (pdf-core/src/page.rs)
  → décodage du flux de contenu (Flate/LZW/ASCII + prédicteurs PNG/TIFF) (pdf-core/src/filters.rs)
  → tokenisation du flux de contenu            (pdf-core/src/content.rs)
  → interprétation (état graphique, chemins, texte, couleur, Form XObjects) (pdf-core/src/interp.rs)
  → DisplayList (chemins + glyphes + position d'images) (pdf-core/src/display.rs)
  → résolution de police (largeurs, Unicode, contours TrueType) (pdf-core/src/font.rs, encoding.rs)
  → rasterisation CPU en PNG                   (pdf-render/src/lib.rs)
```

**Preuve reproductible :**
```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/embedded_truetype_font.pdf /tmp/out.png 0
```
produit un PNG 612×792 avec le texte "AVIL" réellement dessiné (contours de la police Monaco intégrée, pas une image de substitution).

```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/multipage_classic_xref.pdf /tmp/out2.png 0
```
produit un PNG où la phrase "Page 1 - Hello, PDF Manager!" est dessinée avec la **vraie Helvetica système** (substitution macOS : le PDF référence Helvetica sans l'intégrer, cas le plus courant en pratique).

```bash
cargo run --bin pdf-cli -- render-info pdf-core/tests/fixtures/multipage_classic_xref.pdf 0
```
affiche `Recovered text: "Page 1 - Hello, PDF Manager!"` — le texte est reconstruit caractère par caractère via l'encodage réel de la police (pas une supposition).

```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/image_jpeg.pdf /tmp/out3.png 0
```
produit un PNG où une photo JPEG intégrée (filtres chaînés `ASCII85Decode`+`DCTDecode`) est dessinée à la bonne position, taille et orientation.

```bash
cargo run --bin pdf-ui -- pdf-core/tests/fixtures/image_jpeg.pdf
```
ouvre une **vraie fenêtre native** (prototype `egui`/`eframe`) affichant la page rendue, avec navigation page suivante/précédente et zoom (re-rasterisation, pas un agrandissement d'image) — vérifié visuellement par capture d'écran.

```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/embedded_cff_font.pdf /tmp/out4.png 0
```
produit un PNG où "ABC" est dessiné avec de vrais contours **CFF/Type1C** (police STIX intégrée en `/FontFile3`, sous-ensemble de 953 octets) — vérifié visuellement.

```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/image_smask.pdf /tmp/out5.png 0
```
produit un PNG où un carré rouge cramoisi **semi-transparent** (`/SMask`, alpha ~128/255) recouvre un rectangle bleu opaque : la zone de recouvrement est un vrai mélange (violet), pas un rouge plein ni un bleu plein — vérifié visuellement et par assertions de pixels.

```bash
cargo test --workspace -- clip
```
vérifie que le clip (`W`/`W*`) restreint réellement la zone peinte au rendu (`pdf-render`) et que les clips imbriqués s'intersectent puis se restaurent correctement à `Q` (`pdf-core`).

```bash
cargo test -p pdf-app
```
vérifie l'état de session (`pdf-app::Session`) : ouverture de fichier, comptage de pages, navigation avec bornes (`goto_page`/`next_page`/`prev_page` ne dépassent jamais `[0, page_count)`), rendu RGBA de la page courante.

```bash
cargo run --bin pdf-cli -- text pdf-core/tests/fixtures/multipage_classic_xref.pdf 2
```
affiche `Page 3 - Hello, PDF Manager!` — texte extrait par `pdf-text::extract_text` depuis la `DisplayList` de la page 2 (0-based), pas juste concaténé comme dans `render-info` (gère les sauts de ligne).

```bash
cargo test -p pdf-app -- repeated_renders_of_the_same_page_and_scale_reuse_the_cache
```
vérifie que `Session::render_page` réutilise le même `Rc<RenderedPage>` (identité de pointeur) pour deux appels avec le même `(page, échelle)`, mais pas pour une échelle différente.

```bash
cargo run --bin pdf-cli -- render pdf-core/tests/fixtures/rotated_page.pdf /tmp/rotated.png 0
```
produit un PNG **792×612** (dimensions permutées par rapport au 612×792 non pivoté) avec le texte lisible pivoté verticalement, comme un lecteur PDF standard afficherait une page `/Rotate 90` — avant `render_page_rotated`, cette rotation était parsée (`Page::rotate`) mais silencieusement ignorée au rendu.

```bash
cargo run --bin pdf-cli -- dump pdf-core/tests/fixtures/encrypted_rc4.pdf
```
échoue avec `error: encrypted PDF documents are not supported (/Encrypt present in trailer)` — un message clair, là où l'ouverture échouait auparavant plus loin dans le pipeline avec une erreur `FlateDecode: corrupt deflate stream` trompeuse (les chaînes/flux restaient chiffrés).

```bash
cargo run --bin pdf-cli -- text pdf-core/tests/fixtures/cjk_text.pdf 0
```
affiche `你好，世界` — texte chinois simplifié exactement recomposé grâce à la lecture du CMap `/ToUnicode` embarqué dans le fixture (`font.rs::parse_to_unicode_cmap`). Avant ce parseur, cette commande n'affichait rien : le glyphe se dessinait correctement (contour résolu par code brut) mais aucun caractère Unicode n'était récupéré.

```bash
cargo test -p pdf-core -- outline
```
vérifie la lecture de `/Outlines` (`pdf-core::outline`) : arbre plat résolu en index de page sur un fixture réel généré avec `reportlab.Canvas.addOutlineEntry`, arbre imbriqué (`/First` récursif) construit à la main, document sans table des matières -> `[]`.

```bash
cargo test -p pdf-app -- selection_on_current_page_returns_the_requested_text_and_rects
```
vérifie que `Session::selection_on_current_page(9..14)` sur le fixture classique renvoie bien `"Hello"` (et 5 rectangles, un par caractère) — la même mécanique qui alimente la sélection de texte à la souris dans `pdf-ui` (glisser sur la page, surlignage bleu, copie via ⌘C ou le bouton "📋 Copier").

```bash
cargo test -p pdf-render-gpu
```
rastérise via `wgpu`+`lyon` (14 tests) : un rectangle synthétique à la bonne position/couleur, un test d'orientation (contenu haut-gauche de la page -> haut-gauche de l'image, sans flip d'axe explicite contrairement au backend CPU), un chemin tracé (pas rempli, `strokes_a_path_without_filling_its_interior`), une rotation `/Rotate 90` (dimensions permutées + contenu haut-gauche -> haut-droit), le fixture réel `embedded_truetype_font.pdf` (glyphes réellement tessellés et rendus), une image bicolore à la bonne position et un mélange alpha (`/SMask`-like), un clip (`W`/`W*`) qui restreint réellement la zone peinte (stencil buffer) y compris avec deux clips disjoints consécutifs, un glyphe répété à deux positions distinctes servi par le cache `(font, code)`, et un `GpuRenderer` qui rend deux pages successives avec le même `Device`/`Queue`. Si aucun adaptateur `wgpu` n'est disponible dans l'environnement (pas de GPU/driver), ces tests se terminent proprement sans échouer (`eprintln!` + `return`) plutôt que de paniquer.

```bash
cargo test -p pdf-render --test golden
```
compare 23 fixtures réels au rendu de référence (`pdf-render/tests/golden/*.png`) pixel par pixel sous un seuil de tolérance — le harnais de comparaison d'images visé depuis le Sprint 0 (`sprint.md`), qui n'existait pas avant cette passe.

```bash
cargo test -p pdf-render-gpu --test cross_backend
```
compare 12 fixtures réels rendus par `pdf-render` (CPU) et `pdf-render-gpu` (`wgpu`) pixel par pixel — remplace les seules assertions ciblées qui existaient jusque-là pour vérifier la parité entre les deux back-ends.

```bash
cargo bundle --release -p pdf-ui --format osx
mkdir -p /tmp/pdfmanager_dmg_staging && cp -R "target/release/bundle/osx/PDF Manager.app" /tmp/pdfmanager_dmg_staging/
ln -s /Applications /tmp/pdfmanager_dmg_staging/Applications
hdiutil create -volname "PDF Manager" -srcfolder /tmp/pdfmanager_dmg_staging -ov -format UDZO "target/release/bundle/dmg/PDF Manager.dmg"
open "target/release/bundle/osx/PDF Manager.app"
```
produit un `PDF Manager.app` valide (`Info.plist` correct, exécutable Mach-O arm64) puis un `.dmg` (vérifié par `hdiutil verify`, checksum valide) ; l'app se lance et tourne depuis le bundle avec sa vraie barre de menus native (Fichier/Affichage/Fenêtre, voir §2).

```bash
cargo run --bin pdf-cli -- highlight pdf-core/tests/fixtures/multipage_classic_xref.pdf /tmp/highlighted.pdf 0 100 600 300 630 1 1 0
cargo run --bin pdf-cli -- render /tmp/highlighted.pdf /tmp/highlighted.png 0
```
produit un PNG où le rectangle `[100,600]-[300,630]` est rempli en jaune plein — l'annotation `/Highlight` ajoutée par `pdf-edit`, sauvegardée incrémentalement, relue et rendue via son `/AP /N` (voir sprint.md Sprint 13-14).

```bash
cargo run --bin pdf-cli -- fill-form pdf-core/tests/fixtures/acroform_textfield.pdf /tmp/filled.pdf name_field "Ada Lovelace"
cargo run --bin pdf-cli -- render-info /tmp/filled.pdf
```
affiche `Recovered text: "Simple AcroForm testAda Lovelace"` — la valeur du champ, jamais présente dans le PDF original, apparaît dans le texte reconstruit après remplissage + sauvegarde incrémentale + réouverture, preuve que l'apparence régénérée (`/AP /N`) est effectivement dessinée par le rendu normal de la page.

```bash
cargo run --bin pdf-cli -- merge pdf-core/tests/fixtures/multipage_classic_xref.pdf pdf-core/tests/fixtures/embedded_truetype_font.pdf /tmp/merged.pdf
cargo run --bin pdf-cli -- dump /tmp/merged.pdf
cargo run --bin pdf-cli -- split /tmp/merged.pdf /tmp/split.pdf 5
cargo run --bin pdf-cli -- dump /tmp/split.pdf
```
la fusion affiche `Page count: 6` (5 pages d'origine + 1 fusionnée), le découpage de la page fusionnée (indice 5) produit un fichier autonome de `Page count: 1` — la police intégrée de cette page reste résolue après la copie (voir `pdf-edit::extract_pages`).

```bash
cargo run --bin pdf-cli -- delete-page pdf-core/tests/fixtures/multipage_classic_xref.pdf /tmp/del.pdf 2
cargo run --bin pdf-cli -- rotate-page /tmp/del.pdf /tmp/rotated.pdf 0 90
cargo run --bin pdf-cli -- render-info /tmp/rotated.pdf
```
affiche `Page count: 4` puis `Rotate 90` — suppression et rotation persistées par sauvegarde incrémentale, vérifiées après réouverture.

```bash
cargo run --bin pdf-cli -- add-text pdf-core/tests/fixtures/multipage_classic_xref.pdf /tmp/added.pdf 0 50 50 250 80 14 Nouvelle note
cargo run --bin pdf-cli -- replace-text /tmp/added.pdf /tmp/replaced.pdf 0 72 715 400 735 18 Titre remplace
cargo run --bin pdf-cli -- render-info /tmp/replaced.pdf
```
affiche `Recovered text: "Page 1 - Hello, PDF Manager!Nouvelle noteTitre remplace"` — le texte original (`"Page 1..."`) reste extractible malgré le remplacement par superposition (6b, il n'est que recouvert visuellement), aux côtés des deux textes ajoutés (6a et 6b).

**Tests :** 192 tests automatisés (`cargo test --workspace`), tous verts, `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre.

---

## 2. Fait, avec limitations explicites

| Domaine | Ce qui marche | Limitation connue |
|---|---|---|
| Xref | Classique + cross-reference streams (PDF 1.5+) + object streams + chaînes `/Prev` | — |
| Récupération d'erreur | Reconstruction par balayage d'octets si xref corrompue/absente, avec repli sur recherche d'un `/Type /Catalog` | Testé sur un seul fichier corrompu artificiellement, pas sur un corpus de corruptions variées |
| Filtres de flux | FlateDecode, ASCIIHex, ASCII85, LZWDecode, DCTDecode (JPEG, `zune-jpeg`), prédicteurs PNG (types 0-4) et TIFF | Pas de CCITTFaxDecode, JBIG2Decode, JPXDecode |
| Arbre des pages | Parcours récursif `/Pages`→`/Kids`, héritage Resources/MediaBox/Rotate, garde anti-cycle. `/Rotate` est **appliqué au rendu** (`pdf-render::render_page_rotated`, dimensions du pixmap permutées pour 90°/270°) | — |
| Chiffrement | `/Encrypt` détecté à l'ouverture (`Document::open` échoue avec `PdfError::Encrypted`, message clair) | Pas de déchiffrement RC4/AES réel : un PDF chiffré ne peut pas être lu, même avec le bon mot de passe |
| Contenu | ~40 opérateurs : état graphique, chemins, texte, couleur, Form XObjects (récursif, garde de profondeur), **clip (`W`/`W*`) suivi et appliqué** (intersection des clips imbriqués, restauré par `Q`) | Patterns/shadings ignorés ; contenu marqué ignoré |
| Police — largeurs | `/Widths`+`/FirstChar` ; repli sur table AFM Helvetica intégrée si absent | Les 13 autres polices standard (Times, Courier...) n'ont pas de table dédiée, retombent sur une largeur par défaut arbitraire (500/1000 em) |
| Police — encodage | `WinAnsiEncoding`/`StandardEncoding` complets (256 codes) + `/Differences` via un sous-ensemble de l'Adobe Glyph List. **`/ToUnicode` lu et prioritaire** pour les polices simples (`font.rs::parse_to_unicode_cmap`) **et composites** (`parse_to_unicode_cmap_wide`, codes source 2 octets non bornés à 256) : `beginbfchar`/`beginbfrange`, formes base+décalage et tableau explicite | `MacRomanEncoding` est approximé par WinAnsi au-delà de l'ASCII |
| Police — contours | Polices **TrueType intégrées** (`/FontFile2`) et **CFF/Type1C intégrées** (`/FontFile3`, sous-types `Type1C` via `ttf_parser::cff::Table` brut, ou `OpenType` via `ttf_parser::Face`) ; repli cmap Macintosh par code brut si pas de cmap Unicode (TrueType) ou encodage CFF natif par code brut (CFF) ; **substitution système macOS** pour les polices standard non intégrées : Helvetica/Times/Courier/Symbol/ZapfDingbats + alias Arial, sélection de la face gras/italique dans les `.ttc`, cache global des fichiers | Type1 (`/FontFile`, format historique pré-CFF) : aucun contour. Substitution par lecture directe de `/System/Library/Fonts` (chemins macOS codés en dur, pas via l'API Core Text) — non portable en l'état |
| Rendu (CPU, `pdf-render`, référence) | Chemins (fill/stroke/fill+stroke, nonzero/even-odd, courbes de Bézier), glyphes (intégrés **et** substitués), **images décodées avec canal alpha** (JPEG, échantillons bruts 8 bits, `/SMask`), prémultiplication automatique pour `tiny-skia`, conversion Gray/RGB/CMYK, **clip (`W`/`W*`) appliqué** à tous les types d'items (chemins, glyphes, images) via un masque `tiny_skia::Mask` mis en cache par chaîne de clip, **rotation (`/Rotate`) appliquée** | — |
| Rendu (GPU, `pdf-render-gpu`) | Pipeline `wgpu` en parité fonctionnelle avec `pdf-render` : chemins remplis **et tracés** (`lyon::tessellation::{FillTessellator, StrokeTessellator}`), contours de glyphes **mis en cache par `(font, code)`** par appel (`GlyphCache` : tessellation em-space une seule fois, occurrences suivantes juste transformées), **images** (`DisplayItem::Image`, quad texturé `build_image_quad`, même mapping pixel->carré unité->page que côté CPU, alpha straight), **clip (`W`/`W*`) appliqué via un stencil buffer** (`Stencil8` : items groupés par pile de clip comparée par pointeur `Rc`, couches imbriquées accumulées par incrément pour leur intersection, contenu testé contre la profondeur totale), rotation (`/Rotate`) appliquée dans l'espace NDC. **`GpuRenderer`** réutilise un `Device`/`Queue` `wgpu` partagé (`from_shared`) au lieu d'en renégocier un par page — **branché dans `pdf-ui`** (bascule sur `NativeOptions::renderer = Renderer::Wgpu`, `Device`/`Queue` d'`eframe` passés à `pdf_app::Session::set_gpu_renderer`), avec repli automatique et transparent sur `pdf-render` si aucun adaptateur `wgpu` n'est disponible. Validé par 14 tests dédiés (`pdf-render-gpu`) + 1 test `pdf-app` + **12 tests de comparaison pixel automatisée contre `pdf-render`** (`pdf-render-gpu/tests/cross_backend.rs`) | Pipelines/shaders/textures d'une page toujours recréés à chaque appel (coût jugé acceptable, dominé par la tessellation `lyon`) ; pas de vérification visuelle interactive de `pdf-ui` en environnement avec affichage (validé par build/tests uniquement) |
| Images | `DCTDecode` (JPEG) via `zune-jpeg` ; interprétation `/ColorSpace` DeviceGray/RGB/CMYK et `ICCBased` (approximé par `/N`, sans le profil ICC) à 8 bits/composante ; **canal alpha via `/SMask`** (décodé récursivement, rééchantillonné au plus proche voisin si les dimensions diffèrent) ; dessinées à la bonne position/orientation/transparence dans `pdf-render` | Pas de CCITT/JBIG2/JPX, pas d'espaces `Indexed`/`Separation`/`Lab`, pas de 1/2/4/16 bits, pas de `/Mask` (masque de détourage, différent de `/SMask`) |
| Interface graphique (`pdf-ui`) | Fenêtre `egui`/`eframe` fonctionnelle : ouverture de fichier (dialogue natif `rfd`), navigation page suivante/précédente, zoom par re-rasterisation (boutons **et** molette+Ctrl/pincement trackpad via `egui::InputState::zoom_delta`), recherche texte plein document **avec surlignage jaune translucide** des occurrences sur la page affichée, **panneau de miniatures** cliquables (`egui::SidePanel`, une par page, rendues à petite échelle et mises en cache), **panneau de signets/plan** arborescent (indentation par profondeur) qui saute à la page au clic, **mode défilement continu** (toggle "📜 Continu") : toutes les pages empilées verticalement dans une `egui::ScrollArea::show_rows` virtualisée, la recherche/les miniatures/les signets/les boutons précédente-suivante y déclenchent un saut de défilement, **sélection de texte à la souris** (glisser sur la page, surlignage bleu translucide, copie via bouton "📋 Copier" ou ⌘C). Parle à `pdf-app::Session` (état de session : document, page courante, rendu RGBA, recherche, table des matières, sélection) plutôt que directement à `pdf-core`/`pdf-render`. Vérifié visuellement par capture d'écran sur un fixture réel | mode continu : hauteur de ligne uniforme dérivée de la page 0 uniquement (documents à tailles de page hétérogènes mal gérés), sélection de texte limitée au mode page unique (pas en défilement continu), pas de double/triple-clic (mot/ligne) |
| Chrome natif macOS (`pdf-ui/src/native_menu.rs`) | Vraie `NSMenu` système (`objc2`/`objc2-app-kit`), pas dessinée par `egui` : menus Fichier (Ouvrir `⌘O`, Exporter une copie `⌘S`, Fermer `⌘W`), Affichage (Basculer le mode sombre, Plein écran `⌃⌘F`), Fenêtre (Réduire `⌘M`, Zoomer), Quitter `⌘Q`. Actions standard (Quitter/Fermer/Réduire/Zoomer/Plein écran) envoyées le long de la chaîne de répondeurs (cible `nil`, `NSWindow`/`NSApplication` les implémentent déjà) ; actions propres à l'app (Ouvrir/Exporter/Mode sombre) via une classe Objective-C définie côté Rust (`MenuTarget`, `objc2::define_class!`) qui pousse une commande dans un canal MPSC lu à chaque frame. Glisser-déposer un PDF : géré nativement par `egui`/`winit`, sans code Objective-C. Mode sombre : bascule réelle de `NSApplication.appearance` synchronisée avec les couleurs `egui`. Packaging : `cargo-bundle` produit un vrai `.app` (`Info.plist` correct, exécutable Mach-O arm64), empaqueté en `.dmg` via `hdiutil` — vérifié (`hdiutil verify`, l'app tourne depuis le bundle) | Quick Look non fait (extension d'app séparée, hors périmètre) ; `⌘Z`/`⌘⇧Z` (Undo/Redo) volontairement pas câblés — `pdf-edit` a maintenant un vrai journal undo/redo (voir plus bas) mais aucune interface `pdf-ui` ne le déclenche encore ; pas de signature Developer ID ni de notarisation réelle (identifiants Apple Developer non disponibles dans cet environnement, seule une signature ad-hoc du linker existe) |
| Écriture PDF (`pdf-core::writer`) | Sérialise `Object` (scalaires, chaînes en hexadécimal, noms avec échappement `#XX`, tableaux, dictionnaires, flux) en syntaxe PDF — symétrique du lexer/parser. Testé (7 tests) | Pas de compression : tout flux nouvellement créé par ce moteur est écrit sans `/Filter` (choix délibéré de simplicité, pas une limitation de format) |
| Sauvegarde incrémentale (`Document::save_incremental`) | Ajoute des objets nouveaux/modifiés en fin de fichier, suivis d'une nouvelle section xref classique et d'un `trailer` chaîné par `/Prev` à l'ancien `startxref` (ISO 32000-1 §7.5.6) — le fichier original n'est jamais réécrit, seulement complété. `Document::next_free_object_num()` calcule le prochain numéro libre depuis la xref réellement résolue (pas `/Size`, qui peut être incohérent). Testé par un round-trip complet (ajout + mise à jour d'un objet existant, sauvegarde, réouverture, vérification) | Chaque sauvegarde ajoute strictement au fichier, qui grossit à chaque édition (comportement standard d'une sauvegarde incrémentale, pas un bug — voir `export_optimized` ci-dessous pour la compaction) |
| Écriture de PDF autonome (`pdf_core::writer::write_standalone`) | Construit un PDF complet à partir de zéro (xref/trailer neufs, pas un ajout incrémental) — utilisé par le découpage de document (voir plus bas). Gère les trous de numérotation (entrées libres `f`). Testé (réouverture par `Document::open` après écriture) | — |
| Manipulation de pages (`pdf-edit::EditSession`) | `insert_blank_page`/`insert_image_page`/`delete_page`/`move_page`/`rotate_page` opèrent sur l'arbre `/Pages` aplati paresseusement à la première opération (`ensure_flat_page_tree`, attributs hérités `/MediaBox`/`/Rotate`/`/Resources` cuits en dur sur chaque page). `insert_image_page` intègre un JPEG tel quel (`/Filter /DCTDecode`, `pdf_core::filters::jpeg_dimensions` pour la taille sans décoder les pixels). Testé bout en bout (insertion+suppression+déplacement+rotation combinés, sauvegarde, réouverture, vérification de l'ordre/contenu par page ; `undo` spécifiquement) | Pages avec dictionnaire inline dans `/Kids` non supportées (erreur claire) ; PNG/autres formats d'image non gérés (JPEG seulement) ; pas d'interface `pdf-ui` |
| Fusion et découpage de documents (`copy_object_recursive`, `insert_pages_from`/`merge_document`, `extract_pages`) | Copie récursivement un objet et tout ce qu'il référence transitivement (ressources, polices, images, annotations) d'un document source, en renumérotant et en cassant les cycles via une table de correspondance — réutilisé à la fois pour la fusion (copie dans une session existante) et le découpage (`extract_pages`, construit un PDF autonome via `write_standalone`). Testé bout en bout : la police intégrée d'une page fusionnée résout toujours de vrais contours après réouverture (pas une référence dans le vide), le découpage produit le bon sous-ensemble de pages avec le bon contenu | Un seul niveau d'`/AcroForm` entre documents fusionnés (les champs de formulaire d'un document fusionné ne sont pas rattachés à l'`/AcroForm` du document de base) |
| Export/optimisation (`pdf_edit::export_optimized`) | Réécrit le document en entier via `extract_pages` sur toutes ses pages : un vrai garbage collector par reconstruction (seuls les objets atteignables sont copiés), pas une passe séparée. Testé (toutes les pages, dans l'ordre, sans corruption) | Pas de linéarisation (réordonnancement pour l'affichage progressif/streaming) — chantier distinct hors périmètre |
| Annotations & apparences (`pdf-core::interp::run_page_with_annotations`, `pdf-edit`) | Rend chaque annotation visible de `/Annots` via son `/AP /N`, en appliquant l'algorithme de correspondance `BBox`/`Matrix` -> `Rect` (ISO 32000-1 §12.5.5) — testé sur un cas exact (rectangle mappé pixel-parfait) et sur le bit `Hidden`. `pdf-edit::EditSession::add_highlight_annotation` (`/Highlight`) et `add_free_text_annotation` (`/FreeText`, Sprint 17+ 6a) construisent une vraie annotation avec `/AP /N` généré et l'ajoutent à `/Annots` (tableau inline ou objet séparé, les deux gérés) ; `remove_annotation` retire n'importe quelle annotation par indice (générique, pas spécifique à un type). Branché dans `pdf-app::Session` : visible dans `pdf-ui` sans changement côté UI. Validé bout en bout (ajout/retrait, sauvegarde incrémentale, réouverture, rendu) et via `pdf-cli highlight`/`add-text`/`remove-annotation` | Deux types d'annotation (`/Highlight`, `/FreeText`) ; pas de notes/`/Text`, formes, signatures ; pas de vraie transparence (`/ca` non géré par l'interpréteur, rendu en couleur pleine) ; pas d'interface `pdf-ui` pour en créer à la souris |
| Édition de texte, 6a/6b (`pdf-edit::add_free_text_annotation`/`replace_text_with_overlay`) | **6a** (ajout de nouveau texte) et **6b** (remplacement par superposition, "masquer l'ancien + redessiner") partagent la même apparence générée (fond plein optionnel + texte Helvetica). `replace_text_with_overlay` ne touche jamais le flux de contenu original — le texte "caché" reste extractible par `pdf-text`, seulement recouvert visuellement par le rectangle de fond au rendu final. Testé bout en bout : 6a (plus de glyphes qu'avant), 6b (contenu original intact **et** rectangle de recouvrement + nouveau texte présents au rendu), via `pdf-cli add-text`/`replace-text` | **6c** (édition chirurgicale du flux de contenu existant, gestion de subsets de polices) non engagé — traité comme projet de recherche séparé, conformément à sprint.md ; pas d'interface `pdf-ui` ; pas d'édition "en place" d'une annotation déjà créée (retirer + réajouter couvre le même besoin en pratique) |
| Remplissage de formulaires (`pdf-edit::set_form_field_value`) | Trouve un champ par `/T` (un niveau, pas de noms qualifiés par `/Parent`), fixe `/V` et régénère `/AP /N` (police Helvetica non intégrée allouée à la volée, résolue par la substitution système au rendu) — nécessaire car l'interpréteur ne synthétise pas d'apparence à partir de `/V`/`/DA` seuls. Validé bout en bout sur `acroform_textfield.pdf` (`/V` persiste, au moins un glyphe par caractère produit au rendu) et via `pdf-cli fill-form` | Champs texte seulement (pas de cases à cocher/boutons radio/listes) ; pas d'interface `pdf-ui` pour éditer un champ au clic |
| Undo/redo (`pdf-edit::EditOp`) | Capture, pour chaque objet **existant** modifié par une opération, sa valeur avant/après ; `undo`/`redo` restaurent l'une ou l'autre. Testé (`undo_and_redo_toggle_annotation_visibility`, `undo_restores_page_order_after_delete`) | Les objets nouvellement créés par une opération (annotation, apparence, police, page) restent alloués après un `undo` — rendus non référencés donc invisibles/orphelins, pas physiquement supprimés (nettoyage réel = `export_optimized`, voir plus haut, qui ne copie que les objets atteignables) ; pas d'interface `pdf-ui` pour déclencher `undo`/`redo` (le menu natif a des raccourcis prévus mais volontairement pas câblés, voir plus haut) |
| Session applicative (`pdf-app`) | `Session::open`/`page_count`/`page_index`/`goto_page`/`next_page`/`prev_page`/`render_current_page`/`render_page`/`extract_current_page_text`/`find_pages_containing`/`find_matches_on_current_page`/`current_page_media_box`/`page_media_box`/`outline`/`char_index_at_on_current_page`/`selection_on_current_page` — ouverture de fichier, navigation avec bornes, rendu RGBA agnostique du backend (`RenderedPage`, `Rc`-partagé), extraction/recherche/surlignage/sélection de texte, table des matières, **cache de rendu bitmap** (`render_cache`, FIFO 32 entrées, clé `(page, échelle)`), **cache de texte** (`text_cache`) et **cache de la table des matières** (`outline_cache`) par session : une page déjà rastérisée/extraite, ou la table des matières déjà lue, n'est jamais recalculée. Testé unitairement (14 tests, fixture réelle, dont un qui vérifie l'identité de pointeur du cache de rendu et un pour le cache d'outline) | Pas d'historique undo/redo (`EditOp`, Sprint 13-14), pas de multi-documents/onglets, pas d'intégration avec `pdf-edit`, cache de rendu FIFO simple (pas de vraie éviction LRU) |
| Table des matières (`pdf-core::outline`) | `Document::outline()` lit récursivement `/Root /Outlines` (ISO 32000-1 §12.3.3), résout les destinations directes (`/Dest` tableau `[page /Fit ...]`) en index de page via `Page::object_ref` (nouvelle correspondance objet-page ajoutée pour ça), garde anti-cycle sur `/Next`. Testé (3 tests dans `pdf-core`, dont un sur fixture réel généré avec `reportlab.Canvas.addOutlineEntry`) | Destinations nommées (`/Names/Dests`, ancien style `/Root /Dests`) et actions `/A` non `/GoTo` direct : non résolues (`page_index: None`, entrée gardée quand même) ; titres décodés via `Object::as_text_string` (PDFDocEncoding approximé par UTF-8 lossy hors BOM UTF-16) |
| Extraction de texte (`pdf-text`) | `extract_text(&DisplayList) -> String` et `extract_page_text(&DisplayList) -> PageText` (texte + position par caractère) : concatène les glyphes résolus en Unicode dans l'ordre d'émission, insère un saut de ligne quand la ligne de base saute de plus de la moitié de la taille de police (`transform.d`/`transform.e`/`transform.f`). `PageText::find_matches` renvoie un rectangle englobant (fusionné) par occurrence de recherche ; `char_index_at`/`text_in_range`/`rects_in_range` permettent le hit-test position -> caractère et l'extraction d'une plage, utilisés par `pdf-app`/`pdf-ui` pour le surlignage et la sélection de texte. Fonctionne aussi bien avec des polices composites `/Type0` qu'avec des polices simples, `pdf-core::interp` produisant les mêmes `DisplayItem::Glyph` dans les deux cas. Testé (12 tests, dont un sur fixture réel) | Pas de reconstruction par blocs/colonnes, largeur de glyphe approximée (`hauteur_police * 0.6`, pas la vraie largeur — `DisplayItem::Glyph` n'expose pas `/Widths` en sortie), repliement de casse caractère par caractère (pas correct pour les scripts non latins à casse multi-caractères) |
| Polices composites (`/Type0`/CID) | **Gérées** (`pdf-core::font`, voir la doc de module) pour l'`/Encoding` `Identity-H`/`Identity-V` (code source 2 octets = CID directement — couvre l'immense majorité des PDF `/Type0` réels) : `Font::decode_composite` découpe la chaîne en CIDs, `cid_metrics` résout Unicode (`/ToUnicode` via `parse_to_unicode_cmap_wide`) et largeur (`/W`/`/DW`, ISO 32000-1 §9.7.4.3, les deux formes de `/W` gérées), `cid_glyph_outline` résout le contour : `/CIDFontType2` (TrueType) via `/CIDToGIDMap` (`/Identity` ou flux explicite CID->GID) puis la table `glyf` ; `/CIDFontType0` (CFF CID-keyed) via le charset interne de la table CFF elle-même (`ttf_parser::cff::Table::glyph_cid`, inversé au chargement — `/CIDToGIDMap` ne s'applique pas à ce sous-type). `pdf-core::interp::show_text` détecte `Font::is_composite()` et découpe en CIDs 2 octets plutôt qu'en octets. Testé (6 tests dont un bout en bout : même contour que la résolution police simple du même glyphe, un autre qui vérifie que `Tj` émet bien un glyphe par CID et non par octet) | Un `/Encoding` nommé différent (CMap CJK prédéfini comme `UniGB-UCS2-H`) ou un flux de CMap embarqué (plages de largeur variable, `usecmap`) sont traités comme `Identity-H` (code brut = CID) plutôt que rejetés — approximation généralement fausse dans ce cas précis, mais qui tente un rendu plutôt que le repli placeholder complet |
| Texte CJK | Rendu visuel et extraction/recherche fonctionnels aussi bien pour une police simple (`cjk_text.pdf`, TrueType intégrée + `/ToUnicode`) que pour une police composite `/Type0`/CID (voir ci-dessus) — validé bout en bout sur le fixture simple : `你好，世界` recomposé exactement ; validé sur fixture synthétique pour le chemin composite (pas encore de fixture `/Type0` réel dans le corpus, voir §4) | Sans `/ToUnicode` (PDF qui n'en embarque pas), toujours aucun caractère Unicode récupéré, que la police soit simple ou composite |
| Formulaires (`/AcroForm`) | La présence d'un `/AcroForm` n'empêche pas l'ouverture ni le rendu du contenu de la page ; le remplissage de champs texte est fait (voir la ligne `pdf-edit::set_form_field_value` plus haut) | Pas de rendu des widgets comme éléments interactifs cliquables dans `pdf-ui` (édition uniquement via `pdf-edit`/`pdf-cli` pour l'instant) |

---

## 2bis. Grille de conformité PDF (niveaux de compatibilité)

Plutôt que de viser d'emblée un « rendu pixel-perfect de n'importe quel PDF », il est plus utile de suivre la progression par niveaux de complexité croissante. Cette grille reformule ce qui est déjà documenté en §1/§2 sous forme de suivi explicite.

| Niveau | Fonctionnalités | Statut |
|---|---|---|
| **1 — de base** | DeviceGray/RGB/CMYK, chemins (fill/stroke, nonzero/even-odd, Bézier), TrueType/CFF intégrés, JPEG (DCTDecode) + FlateDecode/LZW, texte horizontal, transparence simple (`/SMask`), clip `W`/`W*` | ✅ Fait (voir §1, §2) |
| **2 — intermédiaire** | ~~Type0/CID~~ (fait pour `Identity-H`/`Identity-V`, voir §1/§2), Type1 historique (`/FontFile`), ~~`/ToUnicode`~~ (fait, voir §1/§2), CMaps CJK prédéfinis/embarqués (au-delà d'Identity-H), blend modes, Form XObjects avec groupes de transparence, `Indexed`/`Separation` | 🟡 Entamé |
| **3 — avancé** | Type 3, ICCBased (profil réel, pas juste `/N`), DeviceN, shadings, patterns, groupes de transparence isolés, CCITT/JBIG2/JPX | ❌ Pas fait |

Cette grille sert de repère pour prioriser : le projet est solidement niveau 1 et a bien avancé le niveau 2 (`/ToUnicode`, `/Type0`/CID pour `Identity-H`) ; la prochaine valeur significative de niveau 2 est le Type1 historique (`/FontFile`) ou les CMaps CJK prédéfinis au-delà d'Identity-H.

---

## 3. Ce qui n'existe pas du tout

- **Quick Look** : non fait — nécessiterait une extension d'application séparée (`.qlgenerator`/`appex`, bundle et cible de build Xcode distincts), hors périmètre d'un simple binaire `cargo` (voir §2 pour le reste du chrome natif, qui lui est fait).
- **Signature Developer ID / notarisation Apple** : non faites — nécessitent un compte Apple Developer payant et des identifiants non disponibles dans cet environnement de développement (voir §2, packaging).
- **Logique applicative** (`pdf-app`) : porte l'état de session (voir §2) et le rendu inclut désormais les annotations (`run_page_with_annotations`), mais `pdf-app` n'expose pas encore `pdf-edit` lui-même (pas d'`EditSession` intégrée à `Session`, pas de undo/redo déclenchable depuis `pdf-ui`).
- **Interface utilisateur pour l'édition** : `pdf-edit` a maintenant un vrai moteur (annotations `/Highlight`, remplissage de formulaire texte, undo/redo, sauvegarde incrémentale — voir §2), mais aucune interaction `pdf-ui` ne le déclenche (pas d'outil de surlignage à la souris, pas de clic sur un champ de formulaire). Seules l'API Rust directe et `pdf-cli highlight`/`fill-form` existent.
- **Autres types d'annotation, texte** (section 7 d'architecture.md) : notes, formes, texte libre, signatures, édition du texte existant (Sprint 17+) — non commencés. La manipulation de pages (Sprint 15-16) est faite au niveau moteur, voir §2.
- **Linéarisation** (réordonnancement des objets pour l'affichage progressif/streaming) : non faite, chantier distinct.
- **Extraction de texte par blocs/colonnes** (façon `pdftotext -layout`) : `pdf-text` fait maintenant une reconstruction linéaire avec sauts de ligne heuristiques et des rectangles de position par caractère (voir §2), mais pas de détection de colonnes/tableaux.
- **Décodage d'images** : CCITT, JBIG2, JPX — aucun (JPEG fait, voir §2).
- **Déchiffrement PDF** (`/Encrypt` RC4/AES) : détecté proprement à l'ouverture (voir §2) mais pas déchiffré — aucun PDF chiffré n'est lisible, même avec le bon mot de passe. Signatures numériques, PDF/A : aucun.
- **Packaging macOS** : pas de `.app`, pas de `.dmg`, pas de signature/notarisation.

---

## 4. Corpus de test

25 fixtures PDF dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md) pour le détail et les scripts de régénération), plus `sample_image.jpg` (Sprint 15-16 : une petite image JPEG synthétique, pas un PDF, utilisée pour tester `pdf-edit::insert_image_page`) :
- `minimal.pdf` — PDF minimal fait main.
- `multipage_classic_xref.pdf` / `multipage_xref_stream.pdf` — même contenu (5 pages, texte + rectangle), sauvegardé en xref classique et en xref stream + object streams (PDF 1.5+).
- `corrupted_missing_xref.pdf` — fichier tronqué pour tester la reconstruction par balayage.
- `malformed_wrong_length.pdf` — `/Length` de flux délibérément trop court, un autre mode de corruption réel (auteurs), pour tester la récupération via `endstream` plutôt que la reconstruction par balayage.
- `embedded_truetype_font.pdf` — police Monaco intégrée en TrueType, pour tester l'extraction de contours réelle.
- `image_jpeg.pdf` — photo JPEG intégrée (filtres chaînés `ASCII85Decode`+`DCTDecode`), pour tester le décodage et le rendu d'images.
- `cmyk_jpeg.pdf` — photo JPEG **CMYK** (4 composantes), qui a débusqué et corrigé un vrai bug de décodage (`zune-jpeg` convertit parfois en sortie RGB 3 composantes, en désaccord avec le `/ColorSpace` déclaré).
- `embedded_cff_font.pdf` — police STIX intégrée en CFF/Type1C (`/FontFile3`, sous-ensemble 3 glyphes, construit à la main avec pikepdf faute de support reportlab pour ce format d'embarquement), pour tester l'extraction de contours CFF.
- `image_smask.pdf` — image semi-transparente (`/SMask`, alpha ~128/255) sur un rectangle opaque, pour tester le mélange alpha.
- `indexed_color_image.pdf` — image `/ColorSpace /Indexed`, non supporté : vérifie la dégradation gracieuse (pas de crash, `pixels: None`) plutôt qu'un vrai décodage.
- `rotated_page.pdf` — page `/Rotate 90`, a mis en évidence que la rotation était ignorée au rendu (corrigé, voir §1/§2).
- `landscape_mixed_page_sizes.pdf` — 3 pages de tailles/orientations différentes (Letter, A4 paysage, carré), pour la limite connue du défilement continu (hauteur de ligne dérivée de la page 0).
- `acroform_textfield.pdf` — champ de formulaire texte simple, vérifie que la présence d'un `/AcroForm` n'empêche pas l'ouverture/le rendu.
- `encrypted_rc4.pdf` / `encrypted_aes256.pdf` — PDF chiffrés RC4 40 bits et AES-256, vérifient l'erreur claire `PdfError::Encrypted` pour les deux filtres.
- `incremental_updates_chain.pdf` — 3 mises à jour incrémentales chaînées (`/Prev` x3), pour tester la résolution d'une chaîne plus profonde que le simple niveau déjà couvert.
- `cjk_text.pdf` — texte chinois simplifié en police Songti intégrée (TrueType `glyf`), vérifie l'absence de crash et documente la limite précise (rendu visuel correct, extraction Unicode vide).
- `large_60_pages.pdf` — document 60 pages, pour les tests de navigation/recherche/miniatures à une échelle non triviale.
- `scanned_page_like.pdf` — page pleine occupée par une seule image JPEG sans texte, structure d'un vrai PDF scanné (sans nécessiter CCITT/JBIG2, hors périmètre).
- `pdfa_like_minimal.pdf` — `/Metadata` XMP + `/OutputIntents`, approximation structurelle de PDF/A (pas de validation de conformité complète).
- `bold_italic_standard_fonts.pdf` — polices standard non embarquées gras/italique/Symbol, au-delà du seul Helvetica plain déjà couvert.
- `outline_test.pdf` — 4 pages avec une table des matières plate (une entrée par page), générée avec `reportlab.Canvas.addOutlineEntry`, pour tester la lecture de `/Outlines`.
- `type0_cid_truetype.pdf` — texte "AB" en police composite `/Type0`/`CIDFontType2` (`Identity-H`, `CIDToGIDMap /Identity`), sous-ensemble TrueType Monaco réel, construit à la main avec pikepdf.
- `type0_cid_cff.pdf` — texte "你好" en police composite `/Type0`/`CIDFontType0` (CFF CID-keyed, `/ROS Adobe-GB1`), sous-ensemble réel de Hiragino Sans GB (police système CJK) — comble le manque de fixture réel pour ce chemin de code, jusque-là testé seulement synthétiquement.

Chaque fixture visuel est comparé pixel par pixel à une image de référence (`pdf-render/tests/golden.rs`, `pdf-render-gpu/tests/cross_backend.rs`, voir §1), plus une vérification structurelle spécifique pour les fixtures qui exercent un comportement précis (`pdf-core/src/document.rs`).

Ce corpus (25 fichiers) couvre désormais un représentant de chaque catégorie avancée citée par le critère de sortie de la Phase 1 (architecture.md §9), y compris les deux qui manquaient encore (PDF scanné, PDF/A) et le second type de police composite (`/CIDFontType0` réel). Ce qui reste hors de portée : le volume littéral (« plusieurs centaines de PDF variés ») — obtenir des centaines de PDF *réels* demanderait une source externe (web, jeux de données publics) non accessible depuis cet environnement de développement ; ce n'est donc pas prévu tant que cette contrainte n'est pas levée.

---

## 5. Prochaines étapes logiques (par ordre de valeur/effort)

1. ~~Corpus de test élargi~~ — fait (voir §4) : 25 fixtures couvrant rotation, tailles de page hétérogènes, formulaire, chiffrement RC4/AES-256, CJK (`/CIDFontType2` et `/CIDFontType0`), document 60 pages, chaîne incrémentale à 3 niveaux, corruption par `/Length`, espace colorimétrique non supporté, image CMYK, PDF scanné, PDF/A-like ; a débusqué et corrigé trois bugs réels (`/Rotate` ignoré, erreur trompeuse sur PDF chiffré, JPEG CMYK mal décodé). Seul reste hors périmètre : le volume littéral « plusieurs centaines de PDF variés » (scans/PDF-A réellement produits par des outils tiers), qui demande une source externe non accessible ici.
1bis. ~~Harnais de comparaison pixel (diff + seuil)~~ — fait (`pdf-render/tests/golden.rs`, `pdf-render-gpu/tests/cross_backend.rs`, voir §1) : visé depuis le Sprint 0 (`sprint.md`), jamais construit avant cette passe.
2. ~~`pdf-app` porte l'état de session~~ — fait (voir §2) ; `pdf-edit` a maintenant son propre historique undo/redo (Sprint 13-14, voir §2), mais `pdf-app`/`pdf-ui` ne l'exposent pas encore — reste à intégrer une `EditSession` dans `Session` pour que ce soit déclenchable depuis l'interface.
3. ~~Application du clip (`W`/`W*`)~~ — fait (voir §1, §2).
4. ~~Recherche texte (`pdf-text`) avec surlignage~~ — fait (voir §2) : reconstruction linéaire, recherche plein document, surlignage des occurrences sur la page affichée.
5. ~~Cache du texte et du rendu bitmap par page~~ — fait (voir §2, `Session::text_cache`/`render_cache`).
6. ~~Molette+pincement trackpad pour le zoom~~ — fait (voir §2, `egui::InputState::zoom_delta`).
7. ~~Panneau miniatures~~ — fait (voir §2, `pdf-ui` `SidePanel`).
8. ~~`/ToUnicode`~~ — fait (voir §1/§2, `font.rs::parse_to_unicode_cmap`) : corrige la limite CJK documentée (texte non extractible) pour les polices simples qui l'embarquent, sans nécessiter le support complet des polices composites `/Type0`.
9. ~~Panneau signets/plan (`/Outlines`)~~ — fait (voir §2, `pdf-core::outline` + `Session::outline` + `pdf-ui` `SidePanel` arborescent).
10. ~~Scroll continu entre pages~~ — fait (voir §2, `pdf-ui` mode "📜 Continu", `egui::ScrollArea::show_rows` virtualisée).
11. ~~Sélection de texte à la souris~~ — fait (voir §2, `pdf_text::PageText::char_index_at`/`text_in_range`/`rects_in_range`, `Session::char_index_at_on_current_page`/`selection_on_current_page`, `pdf-ui` glisser-sélectionner + copie ⌘C/bouton).
12. ~~Back-end GPU `wgpu`~~ — fait (voir §2, `pdf-render-gpu`) : parité fonctionnelle avec `pdf-render` (chemins, glyphes avec cache par `(font, code)`, images, clip via stencil buffer, rotation) et branché dans `pdf-ui` (`Device`/`Queue` partagé avec `eframe`, repli automatique sur le CPU). Sprint 9-10 fermé.
13. ~~Polices composites `/Type0`/CID~~ — fait (voir §1/§2, `pdf-core::font`) : codes 2 octets, `/DescendantFonts`, `/W`/`/DW` CID, `/CIDToGIDMap` (TrueType) et charset CFF interne (CID-keyed), `/ToUnicode` grand format. Périmètre restreint à `Identity-H`/`Identity-V` (voir limitation §2). Testé sur fixtures synthétiques (6 tests) **et** deux vrais fixtures : `type0_cid_truetype.pdf` (`/CIDFontType2`, TrueType Monaco) **et** `type0_cid_cff.pdf` (`/CIDFontType0`, CFF CID-keyed, Hiragino Sans GB) — validés visuellement par rendu PNG et par `pdf-cli text`.
14. **CMaps CJK prédéfinis/embarqués au-delà d'`Identity-H`** — prochaine valeur significative de niveau 2 (grille §2bis) : `/Encoding` nommé (`UniGB-UCS2-H`, etc.) ou flux de CMap embarqué avec plages de code de largeur variable. Moins répandu que `Identity-H` en pratique (la plupart des sous-ensembles PDF l'utilisent), mais nécessaire pour les PDF CJK qui référencent un CMap système sans l'embarquer.
15. **Type1 historique (`/FontFile`, pré-CFF)** — dernier trou de niveau 2 (grille §2bis) : nécessite de décoder le chiffrement `eexec` et d'interpréter les charstrings Type1 (jeu d'opcodes différent de Type2/CFF, pas couvert par `ttf-parser`) — un vrai morceau de moteur à écrire, pas juste un test/fixture. Format de moins en moins rencontré en pratique (remplacé par CFF/OpenType).
16. ~~Premier socle d'édition (`pdf-edit`)~~ — fait (voir §2, Sprint 13-14) : sérialisation d'`Object` (`pdf-core::writer`), sauvegarde incrémentale (`Document::save_incremental`), rendu des annotations (`Interpreter::run_page_with_annotations`), et `pdf-edit::EditSession` (annotation `/Highlight`, remplissage de champ de formulaire texte, undo/redo) — tout testé bout en bout (sauvegarde + réouverture + rendu réel).
17. ~~Manipulation de pages (`pdf-edit`)~~ — fait (voir §2, Sprint 15-16) : insertion/suppression/déplacement/rotation de page, fusion/découpage de documents (copie d'objets récursive avec renumérotation), insertion d'image JPEG comme page, export optimisé (garbage collection par reconstruction) — tout testé bout en bout, `pdf-cli` exerçant chaque opération sur un vrai fichier.
18. ~~Édition de texte 6a/6b (`pdf-edit`)~~ — fait (voir §2, Sprint 17+) : ajout de nouveau texte (`/FreeText`) et remplacement par superposition (masquer + redessiner, sans toucher le flux d'origine) — testés bout en bout. 6c (édition chirurgicale du flux existant) volontairement pas engagé, traité comme projet de recherche séparé. **Prochaine valeur significative :** câbler une interface `pdf-ui` pour tout ce qui a été fait aux Sprints 13-14, 15-16 et 17+ (annotations, formulaires, pages, texte) — c'est maintenant le principal écart entre "moteur fonctionnel" et "éditeur utile au quotidien" au sens des jalons de ces sprints ; les raccourcis ⌘Z/⌘⇧Z sont déjà prévus dans le menu natif (Sprint 11-12) mais pas encore branchés.

**Note de découpage (pas une action immédiate) :** `pdf-core` regroupe aujourd'hui lexer, xref, document, interpréteur de contenu et polices. Si ce crate devient difficile à naviguer à mesure qu'il grossit, envisager de le scinder en `pdf-syntax` (lexer/parser/xref bas niveau, sans notion de Page/Font) et `pdf-document` (catalogue/pages/resources) — mais seulement à ce moment-là. Vu la taille actuelle du corpus (25 fixtures) et l'état stub de `pdf-edit`, un découpage préventif ajouterait de la friction sans bénéfice mesurable pour l'instant.

Pour le contexte produit (pourquoi ce projet, contraintes, décisions à trancher), voir [architecture.md §1](./architecture.md#1-objectif-et-périmètre) et [§12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage).
