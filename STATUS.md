# État précis du projet

**Dernière mise à jour :** 2026-07-04 (ajout du harnais de comparaison pixel `pdf-render/tests/golden.rs` + `pdf-render-gpu/tests/cross_backend.rs`, corpus élargi de 15 à 25 fixtures dont un second fixture réel `/Type0` — `type0_cid_cff.pdf`, `/CIDFontType0` CFF CID-keyed, Hiragino Sans GB — et correction d'un bug réel de décodage JPEG CMYK trouvé en construisant ces fixtures, voir sprint.md Sprint 0/3-4/7-8 pour le détail)
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

**Tests :** 128 tests automatisés (`cargo test --workspace`), tous verts, `cargo clippy --workspace --all-targets` sans avertissement.

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
| Rendu (GPU, `pdf-render-gpu`) | Pipeline `wgpu` en parité fonctionnelle avec `pdf-render` : chemins remplis **et tracés** (`lyon::tessellation::{FillTessellator, StrokeTessellator}`), contours de glyphes **mis en cache par `(font, code)`** par appel (`GlyphCache` : tessellation em-space une seule fois, occurrences suivantes juste transformées), **images** (`DisplayItem::Image`, quad texturé `build_image_quad`, même mapping pixel->carré unité->page que côté CPU, alpha straight), **clip (`W`/`W*`) appliqué via un stencil buffer** (`Stencil8` : items groupés par pile de clip comparée par pointeur `Rc`, couches imbriquées accumulées par incrément pour leur intersection, contenu testé contre la profondeur totale), rotation (`/Rotate`) appliquée dans l'espace NDC. **`GpuRenderer`** réutilise un `Device`/`Queue` `wgpu` partagé (`from_shared`) au lieu d'en renégocier un par page — **branché dans `pdf-ui`** (bascule sur `NativeOptions::renderer = Renderer::Wgpu`, `Device`/`Queue` d'`eframe` passés à `pdf_app::Session::set_gpu_renderer`), avec repli automatique et transparent sur `pdf-render` si aucun adaptateur `wgpu` n'est disponible. Validé par 14 tests dédiés (`pdf-render-gpu`) + 1 test `pdf-app` | Pipelines/shaders/textures d'une page toujours recréés à chaque appel (coût jugé acceptable, dominé par la tessellation `lyon`) ; pas de comparaison pixel automatisée entre les deux backends ; pas de vérification visuelle interactive de `pdf-ui` en environnement avec affichage (validé par build/tests uniquement) |
| Images | `DCTDecode` (JPEG) via `zune-jpeg` ; interprétation `/ColorSpace` DeviceGray/RGB/CMYK et `ICCBased` (approximé par `/N`, sans le profil ICC) à 8 bits/composante ; **canal alpha via `/SMask`** (décodé récursivement, rééchantillonné au plus proche voisin si les dimensions diffèrent) ; dessinées à la bonne position/orientation/transparence dans `pdf-render` | Pas de CCITT/JBIG2/JPX, pas d'espaces `Indexed`/`Separation`/`Lab`, pas de 1/2/4/16 bits, pas de `/Mask` (masque de détourage, différent de `/SMask`) |
| Interface graphique (`pdf-ui`) | Fenêtre `egui`/`eframe` fonctionnelle : ouverture de fichier (dialogue natif `rfd`), navigation page suivante/précédente, zoom par re-rasterisation (boutons **et** molette+Ctrl/pincement trackpad via `egui::InputState::zoom_delta`), recherche texte plein document **avec surlignage jaune translucide** des occurrences sur la page affichée, **panneau de miniatures** cliquables (`egui::SidePanel`, une par page, rendues à petite échelle et mises en cache), **panneau de signets/plan** arborescent (indentation par profondeur) qui saute à la page au clic, **mode défilement continu** (toggle "📜 Continu") : toutes les pages empilées verticalement dans une `egui::ScrollArea::show_rows` virtualisée, la recherche/les miniatures/les signets/les boutons précédente-suivante y déclenchent un saut de défilement, **sélection de texte à la souris** (glisser sur la page, surlignage bleu translucide, copie via bouton "📋 Copier" ou ⌘C). Parle à `pdf-app::Session` (état de session : document, page courante, rendu RGBA, recherche, table des matières, sélection) plutôt que directement à `pdf-core`/`pdf-render`. Vérifié visuellement par capture d'écran sur un fixture réel | Pas de menus macOS natifs, pas de raccourcis clavier au-delà de ⌘C, pas de packaging `.app`, mode continu : hauteur de ligne uniforme dérivée de la page 0 uniquement (documents à tailles de page hétérogènes mal gérés), sélection de texte limitée au mode page unique (pas en défilement continu), pas de double/triple-clic (mot/ligne) |
| Session applicative (`pdf-app`) | `Session::open`/`page_count`/`page_index`/`goto_page`/`next_page`/`prev_page`/`render_current_page`/`render_page`/`extract_current_page_text`/`find_pages_containing`/`find_matches_on_current_page`/`current_page_media_box`/`page_media_box`/`outline`/`char_index_at_on_current_page`/`selection_on_current_page` — ouverture de fichier, navigation avec bornes, rendu RGBA agnostique du backend (`RenderedPage`, `Rc`-partagé), extraction/recherche/surlignage/sélection de texte, table des matières, **cache de rendu bitmap** (`render_cache`, FIFO 32 entrées, clé `(page, échelle)`), **cache de texte** (`text_cache`) et **cache de la table des matières** (`outline_cache`) par session : une page déjà rastérisée/extraite, ou la table des matières déjà lue, n'est jamais recalculée. Testé unitairement (14 tests, fixture réelle, dont un qui vérifie l'identité de pointeur du cache de rendu et un pour le cache d'outline) | Pas d'historique undo/redo (`EditOp`, Sprint 13-14), pas de multi-documents/onglets, pas d'intégration avec `pdf-edit`, cache de rendu FIFO simple (pas de vraie éviction LRU) |
| Table des matières (`pdf-core::outline`) | `Document::outline()` lit récursivement `/Root /Outlines` (ISO 32000-1 §12.3.3), résout les destinations directes (`/Dest` tableau `[page /Fit ...]`) en index de page via `Page::object_ref` (nouvelle correspondance objet-page ajoutée pour ça), garde anti-cycle sur `/Next`. Testé (3 tests dans `pdf-core`, dont un sur fixture réel généré avec `reportlab.Canvas.addOutlineEntry`) | Destinations nommées (`/Names/Dests`, ancien style `/Root /Dests`) et actions `/A` non `/GoTo` direct : non résolues (`page_index: None`, entrée gardée quand même) ; titres décodés via `Object::as_text_string` (PDFDocEncoding approximé par UTF-8 lossy hors BOM UTF-16) |
| Extraction de texte (`pdf-text`) | `extract_text(&DisplayList) -> String` et `extract_page_text(&DisplayList) -> PageText` (texte + position par caractère) : concatène les glyphes résolus en Unicode dans l'ordre d'émission, insère un saut de ligne quand la ligne de base saute de plus de la moitié de la taille de police (`transform.d`/`transform.e`/`transform.f`). `PageText::find_matches` renvoie un rectangle englobant (fusionné) par occurrence de recherche ; `char_index_at`/`text_in_range`/`rects_in_range` permettent le hit-test position -> caractère et l'extraction d'une plage, utilisés par `pdf-app`/`pdf-ui` pour le surlignage et la sélection de texte. Fonctionne aussi bien avec des polices composites `/Type0` qu'avec des polices simples, `pdf-core::interp` produisant les mêmes `DisplayItem::Glyph` dans les deux cas. Testé (12 tests, dont un sur fixture réel) | Pas de reconstruction par blocs/colonnes, largeur de glyphe approximée (`hauteur_police * 0.6`, pas la vraie largeur — `DisplayItem::Glyph` n'expose pas `/Widths` en sortie), repliement de casse caractère par caractère (pas correct pour les scripts non latins à casse multi-caractères) |
| Polices composites (`/Type0`/CID) | **Gérées** (`pdf-core::font`, voir la doc de module) pour l'`/Encoding` `Identity-H`/`Identity-V` (code source 2 octets = CID directement — couvre l'immense majorité des PDF `/Type0` réels) : `Font::decode_composite` découpe la chaîne en CIDs, `cid_metrics` résout Unicode (`/ToUnicode` via `parse_to_unicode_cmap_wide`) et largeur (`/W`/`/DW`, ISO 32000-1 §9.7.4.3, les deux formes de `/W` gérées), `cid_glyph_outline` résout le contour : `/CIDFontType2` (TrueType) via `/CIDToGIDMap` (`/Identity` ou flux explicite CID->GID) puis la table `glyf` ; `/CIDFontType0` (CFF CID-keyed) via le charset interne de la table CFF elle-même (`ttf_parser::cff::Table::glyph_cid`, inversé au chargement — `/CIDToGIDMap` ne s'applique pas à ce sous-type). `pdf-core::interp::show_text` détecte `Font::is_composite()` et découpe en CIDs 2 octets plutôt qu'en octets. Testé (6 tests dont un bout en bout : même contour que la résolution police simple du même glyphe, un autre qui vérifie que `Tj` émet bien un glyphe par CID et non par octet) | Un `/Encoding` nommé différent (CMap CJK prédéfini comme `UniGB-UCS2-H`) ou un flux de CMap embarqué (plages de largeur variable, `usecmap`) sont traités comme `Identity-H` (code brut = CID) plutôt que rejetés — approximation généralement fausse dans ce cas précis, mais qui tente un rendu plutôt que le repli placeholder complet |
| Texte CJK | Rendu visuel et extraction/recherche fonctionnels aussi bien pour une police simple (`cjk_text.pdf`, TrueType intégrée + `/ToUnicode`) que pour une police composite `/Type0`/CID (voir ci-dessus) — validé bout en bout sur le fixture simple : `你好，世界` recomposé exactement ; validé sur fixture synthétique pour le chemin composite (pas encore de fixture `/Type0` réel dans le corpus, voir §4) | Sans `/ToUnicode` (PDF qui n'en embarque pas), toujours aucun caractère Unicode récupéré, que la police soit simple ou composite |
| Formulaires (`/AcroForm`) | La présence d'un `/AcroForm` n'empêche pas l'ouverture ni le rendu du contenu de la page | Aucun remplissage ni rendu des widgets en tant qu'éléments interactifs (voir `pdf-edit`, stub vide) |

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

- **Chrome natif macOS** (menus système, raccourcis, glisser-déposer, Quick Look, packaging `.app`/`.dmg`) : aucun. Le prototype `pdf-ui` (voir §2) est une fenêtre `egui`/`eframe` générique, pas une app macOS packagée.
- **Logique applicative** (`pdf-app`) : porte désormais l'état de session (voir §2), mais toujours pas de undo/redo ni d'intégration avec `pdf-edit`.
- **Édition** (`pdf-edit`) : stub vide. Aucune des opérations de la section 7 de architecture.md (pages, annotations, formulaires, texte) n'est implémentée.
- **Extraction de texte par blocs/colonnes** (façon `pdftotext -layout`) : `pdf-text` fait maintenant une reconstruction linéaire avec sauts de ligne heuristiques et des rectangles de position par caractère (voir §2), mais pas de détection de colonnes/tableaux.
- **Décodage d'images** : CCITT, JBIG2, JPX — aucun (JPEG fait, voir §2).
- **Déchiffrement PDF** (`/Encrypt` RC4/AES) : détecté proprement à l'ouverture (voir §2) mais pas déchiffré — aucun PDF chiffré n'est lisible, même avec le bon mot de passe. Signatures numériques, PDF/A : aucun.
- **Packaging macOS** : pas de `.app`, pas de `.dmg`, pas de signature/notarisation.

---

## 4. Corpus de test

15 fixtures dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md) pour le détail et les scripts de régénération) :
- `minimal.pdf` — PDF minimal fait main.
- `multipage_classic_xref.pdf` / `multipage_xref_stream.pdf` — même contenu (5 pages, texte + rectangle), sauvegardé en xref classique et en xref stream + object streams (PDF 1.5+).
- `corrupted_missing_xref.pdf` — fichier tronqué pour tester la reconstruction.
- `embedded_truetype_font.pdf` — police Monaco intégrée en TrueType, pour tester l'extraction de contours réelle.
- `image_jpeg.pdf` — photo JPEG intégrée (filtres chaînés `ASCII85Decode`+`DCTDecode`), pour tester le décodage et le rendu d'images.
- `embedded_cff_font.pdf` — police STIX intégrée en CFF/Type1C (`/FontFile3`, sous-ensemble 3 glyphes, construit à la main avec pikepdf faute de support reportlab pour ce format d'embarquement), pour tester l'extraction de contours CFF.
- `image_smask.pdf` — image semi-transparente (`/SMask`, alpha ~128/255) sur un rectangle opaque, pour tester le mélange alpha.
- `rotated_page.pdf` — page `/Rotate 90`, a mis en évidence que la rotation était ignorée au rendu (corrigé, voir §1/§2).
- `acroform_textfield.pdf` — champ de formulaire texte simple, vérifie que la présence d'un `/AcroForm` n'empêche pas l'ouverture/le rendu.
- `encrypted_rc4.pdf` — PDF chiffré RC4 40 bits, vérifie l'erreur claire `PdfError::Encrypted` (corrigé, voir §1/§2).
- `cjk_text.pdf` — texte chinois simplifié en police Songti intégrée (TrueType `glyf`), vérifie l'absence de crash et documente la limite précise (rendu visuel correct, extraction Unicode vide).
- `large_60_pages.pdf` — document 60 pages, pour les tests de navigation/recherche/miniatures à une échelle non triviale.
- `outline_test.pdf` — 4 pages avec une table des matières plate (une entrée par page), générée avec `reportlab.Canvas.addOutlineEntry`, pour tester la lecture de `/Outlines`.
- `type0_cid_truetype.pdf` — texte "AB" en police composite `/Type0`/`CIDFontType2` (`Identity-H`, `CIDToGIDMap /Identity`), sous-ensemble TrueType Monaco réel (2 glyphes, GID réels du sous-ensembleur `fonttools` utilisés comme codes de contenu), construit à la main avec pikepdf — premier fixture réel `/Type0` du corpus, pour tester bout en bout la résolution CID -> Unicode/largeur/contour sur un PDF produit par un outil tiers plutôt que synthétique.

C'est **loin** du corpus « plusieurs centaines de PDF variés » visé par le critère de sortie de la Phase 1 (architecture.md §9), mais couvre désormais un représentant de chaque catégorie avancée citée par ce critère. Un PDF réellement scanné (image plein page sans couche texte), un PDF/A, et un `/Type0`/`CIDFontType0` réel (CFF CID-keyed, actuellement seulement testé synthétiquement) restent à ajouter ; la variété au sein de chaque catégorie (formulaires avec plusieurs types de champs, PDF chiffrés AES-256, autres scripts CJK/RTL) reste superficielle.

---

## 5. Prochaines étapes logiques (par ordre de valeur/effort)

1. ~~Corpus de test élargi~~ — fait (voir §4) : 13 fixtures couvrant rotation, formulaire, chiffrement, CJK, document 60 pages ; a débusqué et corrigé deux bugs réels (`/Rotate` ignoré, erreur trompeuse sur PDF chiffré). Le corpus « plusieurs centaines de PDF variés » (scans réels, PDF/A, variété au sein de chaque catégorie) reste à faire — plus un manque de volume que de diversité de catégories désormais.
2. ~~`pdf-app` porte l'état de session~~ — fait (voir §2) ; reste à y ajouter l'historique undo/redo quand `pdf-edit` sera implémenté (Sprint 13-14).
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
13. ~~Polices composites `/Type0`/CID~~ — fait (voir §1/§2, `pdf-core::font`) : codes 2 octets, `/DescendantFonts`, `/W`/`/DW` CID, `/CIDToGIDMap` (TrueType) et charset CFF interne (CID-keyed), `/ToUnicode` grand format. Périmètre restreint à `Identity-H`/`Identity-V` (voir limitation §2). Testé sur fixtures synthétiques (6 tests) **et** un vrai fixture (`type0_cid_truetype.pdf`, sous-ensemble TrueType Monaco réel via `fonttools subset`, voir §4) — validé visuellement par rendu PNG (`cargo run --bin pdf-cli -- render`) et par `pdf-cli text`. Un `/CIDFontType0` (CFF CID-keyed) réel reste à ajouter au corpus, le chemin CFF n'étant testé que synthétiquement pour l'instant.
14. **CMaps CJK prédéfinis/embarqués au-delà d'`Identity-H`** — prochaine valeur significative de niveau 2 (grille §2bis) : `/Encoding` nommé (`UniGB-UCS2-H`, etc.) ou flux de CMap embarqué avec plages de code de largeur variable. Moins répandu que `Identity-H` en pratique (la plupart des sous-ensembles PDF l'utilisent), mais nécessaire pour les PDF CJK qui référencent un CMap système sans l'embarquer.

**Note de découpage (pas une action immédiate) :** `pdf-core` regroupe aujourd'hui lexer, xref, document, interpréteur de contenu et polices. Si ce crate devient difficile à naviguer à mesure qu'il grossit, envisager de le scinder en `pdf-syntax` (lexer/parser/xref bas niveau, sans notion de Page/Font) et `pdf-document` (catalogue/pages/resources) — mais seulement à ce moment-là. Vu la taille actuelle du corpus (13 fixtures) et l'état stub de `pdf-edit`, un découpage préventif ajouterait de la friction sans bénéfice mesurable pour l'instant.

Pour le contexte produit (pourquoi ce projet, contraintes, décisions à trancher), voir [architecture.md §1](./architecture.md#1-objectif-et-périmètre) et [§12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage).
