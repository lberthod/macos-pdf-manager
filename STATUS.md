# État précis du projet

**Dernière mise à jour :** 2026-07-04 (lecture du CMap `/ToUnicode` — corrige la limite CJK documentée : `cjk_text.pdf` recompose maintenant exactement le texte source)
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

**Tests :** 96 tests automatisés (`cargo test --workspace`), tous verts, `cargo clippy --workspace --all-targets` sans avertissement.

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
| Police — encodage | `WinAnsiEncoding`/`StandardEncoding` complets (256 codes) + `/Differences` via un sous-ensemble de l'Adobe Glyph List. **`/ToUnicode` lu et prioritaire** (`font.rs::parse_to_unicode_cmap` : `beginbfchar`/`beginbfrange`, formes base+décalage et tableau explicite) pour les polices simples | `MacRomanEncoding` est approximé par WinAnsi au-delà de l'ASCII ; `/ToUnicode` limité aux codes source 1 octet (polices simples, pas `/Type0`) |
| Police — contours | Polices **TrueType intégrées** (`/FontFile2`) et **CFF/Type1C intégrées** (`/FontFile3`, sous-types `Type1C` via `ttf_parser::cff::Table` brut, ou `OpenType` via `ttf_parser::Face`) ; repli cmap Macintosh par code brut si pas de cmap Unicode (TrueType) ou encodage CFF natif par code brut (CFF) ; **substitution système macOS** pour les polices standard non intégrées : Helvetica/Times/Courier/Symbol/ZapfDingbats + alias Arial, sélection de la face gras/italique dans les `.ttc`, cache global des fichiers | Type1 (`/FontFile`, format historique pré-CFF) : aucun contour. Substitution par lecture directe de `/System/Library/Fonts` (chemins macOS codés en dur, pas via l'API Core Text) — non portable en l'état |
| Rendu | Chemins (fill/stroke/fill+stroke, nonzero/even-odd, courbes de Bézier), glyphes (intégrés **et** substitués), **images décodées avec canal alpha** (JPEG, échantillons bruts 8 bits, `/SMask`), prémultiplication automatique pour `tiny-skia`, conversion Gray/RGB/CMYK, **clip (`W`/`W*`) appliqué** à tous les types d'items (chemins, glyphes, images) via un masque `tiny_skia::Mask` mis en cache par chaîne de clip | Pas de GPU |
| Images | `DCTDecode` (JPEG) via `zune-jpeg` ; interprétation `/ColorSpace` DeviceGray/RGB/CMYK et `ICCBased` (approximé par `/N`, sans le profil ICC) à 8 bits/composante ; **canal alpha via `/SMask`** (décodé récursivement, rééchantillonné au plus proche voisin si les dimensions diffèrent) ; dessinées à la bonne position/orientation/transparence dans `pdf-render` | Pas de CCITT/JBIG2/JPX, pas d'espaces `Indexed`/`Separation`/`Lab`, pas de 1/2/4/16 bits, pas de `/Mask` (masque de détourage, différent de `/SMask`) |
| Interface graphique (`pdf-ui`) | Fenêtre `egui`/`eframe` fonctionnelle : ouverture de fichier (dialogue natif `rfd`), navigation page suivante/précédente, zoom par re-rasterisation (boutons **et** molette+Ctrl/pincement trackpad via `egui::InputState::zoom_delta`), recherche texte plein document **avec surlignage jaune translucide** des occurrences sur la page affichée, **panneau de miniatures** cliquables (`egui::SidePanel`, une par page, rendues à petite échelle et mises en cache). Parle à `pdf-app::Session` (état de session : document, page courante, rendu RGBA, recherche) plutôt que directement à `pdf-core`/`pdf-render`. Vérifié visuellement par capture d'écran sur un fixture réel | Pas de menus macOS natifs, pas de raccourcis clavier, pas de sélection de texte à la souris, pas de panneau signets/plan, pas de scroll continu entre pages, pas de packaging `.app` |
| Session applicative (`pdf-app`) | `Session::open`/`page_count`/`page_index`/`goto_page`/`next_page`/`prev_page`/`render_current_page`/`render_page`/`extract_current_page_text`/`find_pages_containing`/`find_matches_on_current_page`/`current_page_media_box` — ouverture de fichier, navigation avec bornes, rendu RGBA agnostique du backend (`RenderedPage`, `Rc`-partagé), extraction/recherche/surlignage de texte, **cache de rendu bitmap** (`render_cache`, FIFO 32 entrées, clé `(page, échelle)`) et **cache de texte** (`text_cache`) par page : une page déjà rastérisée ou déjà extraite n'est jamais recalculée pour les mêmes paramètres. Testé unitairement (10 tests, fixture réelle, dont un qui vérifie l'identité de pointeur du cache de rendu) | Pas d'historique undo/redo (`EditOp`, Sprint 13-14), pas de multi-documents/onglets, pas d'intégration avec `pdf-edit`, cache de rendu FIFO simple (pas de vraie éviction LRU) |
| Extraction de texte (`pdf-text`) | `extract_text(&DisplayList) -> String` et `extract_page_text(&DisplayList) -> PageText` (texte + position par caractère) : concatène les glyphes résolus en Unicode dans l'ordre d'émission, insère un saut de ligne quand la ligne de base saute de plus de la moitié de la taille de police (`transform.d`/`transform.e`/`transform.f`). `PageText::find_matches` renvoie un rectangle englobant (fusionné) par occurrence de recherche, utilisé par `pdf-app`/`pdf-ui` pour le surlignage. Testé (8 tests, dont un sur fixture réel) | Pas de reconstruction par blocs/colonnes, largeur de glyphe approximée (`hauteur_police * 0.6`, pas la vraie largeur — `DisplayItem::Glyph` n'expose pas `/Widths` en sortie), repliement de casse caractère par caractère (pas correct pour les scripts non latins à casse multi-caractères), hérite des limites de résolution Unicode de `pdf-core::font` (toujours pas de polices composites `/Type0`, même avec `/ToUnicode` lu) |
| Polices composites | Détection de `/Type0` (`Font::is_composite()`) | Aucune gestion réelle : codes 2 octets, `/DescendantFonts`, `/W` CID — tout retombe sur le comportement placeholder (pas d'Unicode, largeur par défaut) |
| Texte CJK (police simple, non composite) | Rendu visuel correct quand une police TrueType `glyf` est intégrée (contour résolu par code brut via le `cmap` de la police) **et** extraction/recherche fonctionnelles quand un `/ToUnicode` est présent (cas du fixture `cjk_text.pdf`, généré par reportlab qui l'embarque par défaut pour les sous-ensembles TrueType) — validé bout en bout : `你好，世界` recomposé exactement | Sans `/ToUnicode` (PDF qui n'en embarque pas), toujours aucun caractère Unicode récupéré ; polices composites `/Type0`/CID (CJK multi-octets) toujours pas gérées |
| Formulaires (`/AcroForm`) | La présence d'un `/AcroForm` n'empêche pas l'ouverture ni le rendu du contenu de la page | Aucun remplissage ni rendu des widgets en tant qu'éléments interactifs (voir `pdf-edit`, stub vide) |

---

## 2bis. Grille de conformité PDF (niveaux de compatibilité)

Plutôt que de viser d'emblée un « rendu pixel-perfect de n'importe quel PDF », il est plus utile de suivre la progression par niveaux de complexité croissante. Cette grille reformule ce qui est déjà documenté en §1/§2 sous forme de suivi explicite.

| Niveau | Fonctionnalités | Statut |
|---|---|---|
| **1 — de base** | DeviceGray/RGB/CMYK, chemins (fill/stroke, nonzero/even-odd, Bézier), TrueType/CFF intégrés, JPEG (DCTDecode) + FlateDecode/LZW, texte horizontal, transparence simple (`/SMask`), clip `W`/`W*` | ✅ Fait (voir §1, §2) |
| **2 — intermédiaire** | Type0/CID + CMaps, Type1 historique (`/FontFile`), ~~`/ToUnicode`~~ (fait, voir §1/§2), blend modes, Form XObjects avec groupes de transparence, `Indexed`/`Separation` | 🟡 Entamé |
| **3 — avancé** | Type 3, ICCBased (profil réel, pas juste `/N`), DeviceN, shadings, patterns, groupes de transparence isolés, CCITT/JBIG2/JPX | ❌ Pas fait |

Cette grille sert de repère pour prioriser : le projet est solidement niveau 1 et a commencé le niveau 2 (`/ToUnicode`) ; la prochaine valeur significative est les polices composites `/Type0`/CID (CJK/vietnamien multi-octets, très répandu en pratique).

---

## 3. Ce qui n'existe pas du tout

- **Chrome natif macOS** (menus système, raccourcis, glisser-déposer, Quick Look, packaging `.app`/`.dmg`) : aucun. Le prototype `pdf-ui` (voir §2) est une fenêtre `egui`/`eframe` générique, pas une app macOS packagée.
- **Logique applicative** (`pdf-app`) : porte désormais l'état de session (voir §2), mais toujours pas de undo/redo ni d'intégration avec `pdf-edit`.
- **Édition** (`pdf-edit`) : stub vide. Aucune des opérations de la section 7 de architecture.md (pages, annotations, formulaires, texte) n'est implémentée.
- **Extraction de texte par blocs/colonnes** (façon `pdftotext -layout`) : `pdf-text` fait maintenant une reconstruction linéaire avec sauts de ligne heuristiques et des rectangles de position par caractère (voir §2), mais pas de détection de colonnes/tableaux.
- **Panneau signets/plan** (`/Outlines`) : aucun. Le panneau miniatures existe (voir §2) mais pas de lecture de la table des matières du PDF.
- **Décodage d'images** : CCITT, JBIG2, JPX — aucun (JPEG fait, voir §2).
- **Déchiffrement PDF** (`/Encrypt` RC4/AES) : détecté proprement à l'ouverture (voir §2) mais pas déchiffré — aucun PDF chiffré n'est lisible, même avec le bon mot de passe. Signatures numériques, PDF/A : aucun.
- **Packaging macOS** : pas de `.app`, pas de `.dmg`, pas de signature/notarisation.

---

## 4. Corpus de test

13 fixtures dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md) pour le détail et les scripts de régénération) :
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

C'est **loin** du corpus « plusieurs centaines de PDF variés » visé par le critère de sortie de la Phase 1 (architecture.md §9), mais couvre désormais un représentant de chaque catégorie avancée citée par ce critère. Un PDF réellement scanné (image plein page sans couche texte) et un PDF/A restent à ajouter ; la variété au sein de chaque catégorie (formulaires avec plusieurs types de champs, PDF chiffrés AES-256, autres scripts CJK/RTL) reste superficielle.

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
9. **Sprint 9-10 restant** — back-end GPU `wgpu` (tessellation `lyon`, atlas de glyphes), scroll continu entre pages, panneau signets/plan (`/Outlines`), sélection de texte à la souris.
10. **Polices composites `/Type0`/CID** — prochaine valeur significative de niveau 2 (grille §2bis) : codes 2 octets, `/DescendantFonts`, `/W` CID. Très répandu en pratique pour le CJK réel (contrairement au fixture de test qui utilise une police simple).

**Note de découpage (pas une action immédiate) :** `pdf-core` regroupe aujourd'hui lexer, xref, document, interpréteur de contenu et polices. Si ce crate devient difficile à naviguer à mesure qu'il grossit, envisager de le scinder en `pdf-syntax` (lexer/parser/xref bas niveau, sans notion de Page/Font) et `pdf-document` (catalogue/pages/resources) — mais seulement à ce moment-là. Vu la taille actuelle du corpus (13 fixtures) et l'état stub de `pdf-edit`, un découpage préventif ajouterait de la friction sans bénéfice mesurable pour l'instant.

Pour le contexte produit (pourquoi ce projet, contraintes, décisions à trancher), voir [architecture.md §1](./architecture.md#1-objectif-et-périmètre) et [§12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage).
