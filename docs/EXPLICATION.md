# Comment fonctionne le moteur — explication détaillée

**Dernière mise à jour :** 2026-07-04
**Public :** développeur qui rejoint le projet et veut comprendre précisément ce que fait chaque couche, avec les vrais noms de fichiers/fonctions et le cheminement exact des données. Complément de [architecture.md](../architecture.md) (la cible à long terme) et [STATUS.md](../STATUS.md) (l'état à date).

---

## 1. Vue d'ensemble : que se passe-t-il quand on rend une page ?

Commande de référence :

```bash
cargo run --bin pdf-cli -- render fichier.pdf sortie.png 0
```

Le cheminement complet, étape par étape :

```
fs::read("fichier.pdf")                      → Vec<u8>
Document::open(bytes)                        → xref + trailer résolus
doc.page(0)                                  → Page (MediaBox, Rotate, Resources)
doc.page_content(&page)                      → flux de contenu décodé (Vec<u8>)
Interpreter::run_page(doc, resources, ...)   → DisplayList (chemins, glyphes, images)
pdf_render::render_page(&display, media_box) → Pixmap 612×792
pdf_render::encode_png(&pixmap)              → bytes PNG
```

Chaque flèche est détaillée dans les sections suivantes.

---

## 2. Couche 1 — Lexer ([pdf-core/src/lexer.rs](../pdf-core/src/lexer.rs))

Un PDF est un fichier **binaire** dont certaines zones sont du texte structuré. Le lexer découpe une tranche `&[u8]` en tokens :

| Entrée brute | Token produit |
|---|---|
| `42`, `-7`, `3.25` | `Integer(42)`, `Integer(-7)`, `Real(3.25)` |
| `(Hello \(world\))` | `LiteralString(bytes)` — échappements `\n \t \( \)` et octaux `\101` résolus |
| `<48656C6C6F>` | `HexString(bytes)` |
| `/Name`, `/A#42` | `Name("Name")`, `Name("AB")` — `#xx` décodé |
| `<< >>`, `[ ]` | `DictStart/DictEnd`, `ArrayStart/ArrayEnd` |
| `obj`, `endobj`, `stream`, `R`, `true`... | `Keyword(String)` |
| `% commentaire` | (ignoré) |

Points de conception importants :
- **Jamais de `&str` en entrée** : un PDF contient des octets arbitraires ; le lexer travaille octet par octet et n'assume l'UTF-8 nulle part.
- **Tolérance** : nombres dégénérés (`4.`, `-.5`) acceptés, mots-clés inconnus renvoyés tels quels plutôt qu'en erreur.
- Le lexer est **positionnable** (`with_pos`, `seek`, `pos`) parce que le format PDF exige des sauts : la xref donne des offsets absolus d'objets, les flux `stream` imposent de sauter leurs données binaires.

## 3. Couche 2 — Parser d'objets ([pdf-core/src/parser.rs](../pdf-core/src/parser.rs))

Transforme les tokens en objets **COS** (le modèle de données PDF, [pdf-core/src/object.rs](../pdf-core/src/object.rs)) :

```rust
pub enum Object {
    Null, Boolean(bool), Integer(i64), Real(f64),
    String(Vec<u8>), Name(String),
    Array(Vec<Object>), Dictionary(Dictionary),
    Stream(Stream),          // Dictionary + données brutes
    Reference(ObjRef),       // « 12 0 R »
}
```

Deux subtilités qui justifient l'existence du `Parser` au-dessus du `Lexer` :

1. **Les références indirectes `N G R`** : quand le parser lit un entier, il ne sait pas encore si c'est une valeur (`42`) ou le début d'une référence (`42 0 R`). Il faut **deux tokens de lookahead** (le buffer `buffered` du parser) pour trancher. Exemple concret : dans `[1 2 3 0 R 7]`, les éléments sont `1`, `2`, `Reference(3,0)`, `7`.

2. **Les flux `stream`** : après un dictionnaire, le mot-clé `stream` indique que des données binaires suivent. Leur longueur vient de `/Length` (quand c'est un entier direct) ; sinon on cherche littéralement `endstream`. Les données ne sont **pas** tokenisées — elles sont copiées telles quelles dans `Stream::raw_data`.

## 4. Couche 3 — Xref ([pdf-core/src/xref.rs](../pdf-core/src/xref.rs))

La table de références croisées dit **où trouver chaque objet** dans le fichier. Trois chemins possibles, essayés dans cet ordre :

### 4.1 Xref classique (`xref` ... `trailer`)
Lire `startxref` en fin de fichier → offset de la section xref → entrées `offset génération n/f` par sous-section → dictionnaire `trailer`. Les chaînes `/Prev` (mises à jour incrémentales) sont suivies, avec garde anti-boucle ; les entrées les plus récentes priment.

### 4.2 Cross-reference stream (PDF 1.5+)
Si l'offset `startxref` ne pointe pas sur le mot-clé `xref`, c'est un **objet stream** `/Type /XRef` : ses données (souvent FlateDecode + prédicteur PNG "Up") contiennent des enregistrements binaires à largeur fixe décrits par `/W [w0 w1 w2]` :
- type 0 : objet libre (ignoré)
- type 1 : `XrefEntry::Offset(offset)` — objet à un offset direct
- type 2 : `XrefEntry::Compressed { stream_num, index }` — objet **compressé dans un object stream** (`/Type /ObjStm`)

### 4.3 Reconstruction de secours (`reconstruct_by_scan`)
Si tout échoue (xref corrompue, `startxref` absent), on **scanne le fichier au niveau des octets** à la recherche des motifs `N G obj`. Leçon apprise en le développant : un scan basé sur le lexer échoue sur les fichiers réels, car les données binaires des flux compressés contiennent des octets qui ressemblent à des délimiteurs PDF — d'où le scan brut, avec lecture *à reculons* du numéro d'objet (`parse_header_backwards`). Si aucun trailer exploitable n'est trouvé, on cherche un objet `/Type /Catalog` et on synthétise un trailer minimal.

## 5. Résolution d'objets ([pdf-core/src/document.rs](../pdf-core/src/document.rs))

`Document::resolve(ObjRef)` est le point d'accès unique au graphe d'objets :

- **`XrefEntry::Offset`** → positionner le parser à l'offset, lire `N G obj ... endobj`.
- **`XrefEntry::Compressed`** → résoudre d'abord l'object stream conteneur (récursion), décoder son flux, lire son en-tête (paires `numéro offset` × `/N`), puis parser l'objet à `/First + offset_relatif`.
- **Cache** (`RefCell<BTreeMap<u32, Object>>`) : chaque objet n'est parsé qu'une fois par document.
- `Document::get(&Object)` est le raccourci utilisé partout : si l'objet est une `Reference`, elle est résolue ; sinon il est retourné tel quel.

## 6. Filtres de flux ([pdf-core/src/filters.rs](../pdf-core/src/filters.rs))

`decode_stream(&Stream)` applique la chaîne `/Filter` (un nom ou un tableau de noms), puis le prédicteur `/DecodeParms` correspondant à chaque filtre :

| Filtre | Implémentation |
|---|---|
| `FlateDecode` | crate `flate2` (zlib) — décision assumée, voir architecture.md §4.3 |
| `ASCIIHexDecode`, `ASCII85Decode` | maison |
| `LZWDecode` | maison — variante PDF (clear=256, eod=257, largeur 9→12 bits, *early change*) |
| Prédicteur PNG (types 0-4 : None/Sub/Up/Average/Paeth) | maison — indispensable : les xref streams l'utilisent quasi systématiquement |
| Prédicteur TIFF (2) | maison |
| `DCTDecode` (JPEG) | crate `zune-jpeg` (pur Rust) — sortie interleavée dont le nombre de composantes dépend du JPEG (1/3/4), interprétée par `image.rs` |
| `CCITTFax`, `JBIG2`, `JPX` | **pas implémentés** → `PdfError::UnsupportedFilter` |

## 7. Arbre des pages ([pdf-core/src/page.rs](../pdf-core/src/page.rs))

`Document::pages()` parcourt récursivement `/Root → /Pages → /Kids`. Deux règles PDF non évidentes gérées ici :

- **Héritage** : `Resources`, `MediaBox` et `Rotate` peuvent être portés par un nœud `/Pages` parent et hérités par toutes ses feuilles. Le parcours propage un struct `Inherited` raffiné à chaque niveau.
- **Robustesse** : garde anti-cycle sur les références (un `/Kids` malformé peut pointer vers un ancêtre), nœuds non-dictionnaires ignorés, `/Type` manquant traité comme `/Page`.

`Document::page_content(&Page)` concatène les flux `/Contents` (un seul stream **ou** un tableau de streams — les deux formes sont légales) et les décode via `filters`.

## 8. Interprétation du contenu ([pdf-core/src/content.rs](../pdf-core/src/content.rs) + [interp.rs](../pdf-core/src/interp.rs))

### 8.1 Tokenisation (`content.rs`)
Le flux de contenu est un mini-langage **postfixé** : les opérandes précèdent l'opérateur (`72 720 Td`, `(Hello) Tj`). `parse_content_stream` réutilise le lexer (un flux de contenu ne contient jamais de référence `N G R`, donc pas besoin du parser lourd) et produit des `ContentInstruction { operator, operands }`. Les **images inline** (`BI ... ID <binaire> EI`) sont détectées et leurs octets sautés — via `/L` quand présent, sinon recherche heuristique de `EI` entouré de blancs.

### 8.2 Exécution (`interp.rs`)
`Interpreter::run_page` maintient :
- un **état graphique** (`GraphicsState`) : matrice CTM, couleurs fill/stroke, épaisseur de trait, paramètres texte (`Tc/Tw/Tz/TL/Ts`), police courante — empilé/dépilé par `q`/`Q` ;
- deux **matrices texte** (`text_matrix`, `text_line_matrix`) manipulées par `Td/TD/Tm/T*` et avancées à chaque glyphe ;
- le **chemin courant** en construction (`m l c v y re h`), transformé par la CTM au moment de la construction, puis émis dans la DisplayList par un opérateur de peinture (`f`, `S`, `B`, `n`...).

La sortie est une **DisplayList** ([display.rs](../pdf-core/src/display.rs)) : une liste plate de primitives déjà résolues, indépendante du langage PDF :

```rust
pub enum DisplayItem {
    Path  { segments, paint, fill_rule, fill_color, stroke_color, line_width, sets_clip },
    Glyph { font, code, unicode, transform, color, advance_is_estimated, outline },
    Image { resource, transform, pixels },
}
```

`outline` (contour de glyphe) et `pixels` (bitmap RGBA8 décodé) sont déjà résolus au moment de l'interprétation — la DisplayList reste auto-suffisante, `pdf-render` n'a plus besoin de retoucher au document ou aux polices.

Les **Form XObjects** (`Do`) sont interprétés récursivement (leur `/Matrix` est composée avec la CTM, leurs `/Resources` sont empilées), avec une garde de profondeur contre les formes auto-référentes.

### 8.3 La convention matricielle (piège classique)
Les matrices PDF sont en convention **vecteur-ligne** : `[x' y' 1] = [x y 1] · M`. Dans le code, `a.then(&b)` signifie « appliquer `a` puis `b` », soit le produit `a × b` dans cette convention. La transformation finale d'un glyphe est :

```
scale(font_size) → text_matrix → CTM
```

## 9. Polices ([pdf-core/src/font.rs](../pdf-core/src/font.rs) + [encoding.rs](../pdf-core/src/encoding.rs))

C'est la couche qui transforme un **code de caractère brut** (un octet dans `(Hello) Tj`) en trois informations :

1. **Un caractère Unicode** — via `/Encoding` : tables `WinAnsiEncoding`/`StandardEncoding` complètes, surchargées par `/Differences` (noms de glyphes résolus via un sous-ensemble de l'Adobe Glyph List : `eacute` → é, `uni00E9` → é...).
2. **Une largeur d'avance** (millièmes d'em) — via `/Widths` + `/FirstChar` ; à défaut, table AFM Helvetica intégrée en dur (les polices standard ne portent pas de `/Widths` : le lecteur est censé les connaître) ; à défaut, 500/1000.
3. **Un contour vectoriel** (`glyph_outline`) — trois sources, essayées dans cet ordre :
   - **Police intégrée TrueType** `/FontFile2` : décodée puis parsée par `ttf_parser::Face`. Résolution du glyphe par cmap Unicode, avec **repli sur le cmap Macintosh interrogé par code brut** — les sous-ensembles générés par reportlab & co n'ont souvent *que* ce cmap. Les courbes quadratiques TrueType sont élevées en cubiques pour rester homogènes avec le pipeline.
   - **Police intégrée CFF/Type1C** `/FontFile3` : deux sous-cas selon le `/Subtype` du flux lui-même — `OpenType` (conteneur sfnt complet, réutilise le chemin `ttf_parser::Face` ci-dessus, qui gère glyf et CFF de façon transparente) ou `Type1C`/`CIDFontType0C` (CFF **brut**, sans conteneur sfnt : parsé directement via `ttf_parser::cff::Table::parse`, qui fonctionne sur ces octets tels quels). Résolution du glyphe par l'encodage/charset natif de la table CFF (`Table::glyph_index(code)`, par code brut — la CFF n'a pas de notion de cmap Unicode). Mise à l'échelle via la FontMatrix de la table (`sx`, `0.001` dans l'immense majorité des fontes).
   - **Substitution système** (module `system_font`) : pour les polices standard non intégrées, mapping nom → fichier de `/System/Library/Fonts` (Helvetica/Times/Courier `.ttc`, Symbol, ZapfDingbats ; Arial → Helvetica), sélection de la face gras/italique dans la collection, cache global des fichiers (un `.ttc` fait ~2 Mo, on ne le lit qu'une fois par processus). Les préfixes de sous-ensemble `ABCDEF+Nom` sont retirés avant le mapping.

Les contours sont émis en **espace em normalisé** (1.0 = taille de police) ; c'est le renderer qui applique `transform` (qui contient déjà taille × matrice texte × CTM).

Ce qui n'est **pas** géré : polices composites `/Type0`/CID (codes 2 octets — CJK), `/ToUnicode`, Type1 historique (`/FontFile`, le format pré-CFF d'Adobe — rare aujourd'hui). Dans ces cas le pipeline dégrade proprement : glyphe sans contour (non dessiné), largeur placeholder signalée par `advance_is_estimated`.

## 10. Images ([pdf-core/src/image.rs](../pdf-core/src/image.rs))

Une image XObject (`/Subtype /Image`) est décodée en deux temps, au moment de l'interprétation (`interp.rs::do_xobject`), pour que la DisplayList porte des pixels déjà prêts à dessiner :

1. **`filters::decode_stream`** décompresse le flux — pour `DCTDecode`, ça veut dire décoder le JPEG entier via `zune-jpeg`, qui renvoie des échantillons entrelacés (1 composante = niveaux de gris, 3 = RGB, 4 = CMYK selon le JPEG source).
2. **`image::decode_image`** interprète ces octets à la lumière de `/ColorSpace` (`DeviceGray`/`DeviceRGB`/`DeviceCMYK`, `CalGray`/`CalRGB` traités pareil, `ICCBased` approximé par son nombre de composantes `/N` — le profil ICC lui-même n'est pas lu) et les convertit en RGBA8 (alpha toujours 255).

Un échec de décodage (format non supporté, dimensions incohérentes) ne fait pas échouer toute la page : `do_xobject` capture l'erreur et pose `pixels: None`, que `pdf-render` traite comme « rien à dessiner ».

Le canal alpha (`/SMask`, masque de fondu en niveaux de gris) est géré : décodé récursivement comme une image `DeviceGray` à part entière, rééchantillonné au plus proche voisin si ses dimensions diffèrent de l'image principale, puis placé dans le canal alpha du RGBA8 straight (non prémultiplié) — c'est `pdf-render` qui prémultiplie avant de construire le pixmap `tiny-skia` (voir §11).

**Non géré** : `CCITTFaxDecode`/`JBIG2Decode`/`JPXDecode`, espaces `Indexed`/`Separation`/`Lab`, profondeurs autres que 8 bits/composante, `/Mask` (masque de détourage, différent de `/SMask`), `/ImageMask`.

## 11. Rendu ([pdf-render/src/lib.rs](../pdf-render/src/lib.rs))

`render_page(&DisplayList, media_box)` rasterise via `tiny-skia` :

- **Dimension** : `render_page` fait 1 point PDF = 1 pixel ; `render_page_scaled(display, media_box, scale)` re-rasterise à `scale` fois cette résolution (utilisé pour le zoom dans `pdf-ui` — pas un agrandissement d'image a posteriori, la page est réinterprétée à la résolution demandée).
- **Inversion d'axe** : l'espace PDF a l'origine en **bas-gauche**, Y vers le haut ; le pixmap a l'origine en haut-gauche. Tous les points passent par `flip`.
- **Chemins** : `fill_path`/`stroke_path` avec la règle de remplissage PDF correspondante (nonzero → Winding, even-odd → EvenOdd), anti-aliasing activé.
- **Glyphes** : le contour em-normalisé est transformé par la matrice du glyphe (`transform.apply`) puis rempli. Pas de hinting, pas d'atlas — correct d'abord, rapide ensuite (le GPU/wgpu est prévu Phase 3).
- **Images** : le bitmap RGBA8 décodé est composé de trois transformations enchaînées (`Matrix::then`) : pixel→carré unité (mise à l'échelle + inversion, la ligne 0 des données étant le *haut* de l'image), carré unité→espace page (`DisplayItem::Image::transform`, la CTM au moment du `Do`), puis page→pixmap (le même flip que les chemins). Le tout est converti en `tiny_skia::Transform` et passé à `draw_pixmap`. Le RGBA fourni par `pdf-core` est **straight** (non prémultiplié) ; `premultiply_if_needed` le convertit avant `draw_pixmap` (`tiny-skia` attend du prémultiplié), sans copie dans le cas courant où l'image est entièrement opaque (pas de `/SMask`).
- **Couleurs** : Gray/RGB directs ; CMYK converti naïvement (`(1-c)(1-k)`...) sans profil ICC.

## 12. Interface graphique ([pdf-ui/src/main.rs](../pdf-ui/src/main.rs))

Prototype `egui`/`eframe` — le pipeline `Document::open → page → page_content → Interpreter::run_page → pdf_render::render_page_scaled` tourne à chaque frame où la page ou le zoom courant a changé (`ViewerApp::ensure_texture`, qui compare `(page_index, zoom_quantifié)` à ce qui a produit la texture affichée pour éviter de re-rasteriser inutilement).

Points d'implémentation notables :
- **Conversion Pixmap → texture egui** : `tiny_skia::Pixmap::data()` est passé tel quel à `egui::ColorImage::from_rgba_unmultiplied`. Ça fonctionne sans conversion premultiplied/straight parce que `render_to_pixmap` peint toujours sur un fond blanc **opaque** : après compositing "over" sur un fond d'alpha 1, le résultat a toujours alpha=255 partout, donc premultiplied et straight coïncident.
- **Raccourci assumé** : ce prototype parle directement à `pdf-core`/`pdf-render`, en court-circuitant `pdf-app` (toujours un stub). C'est exactement ce que suggère architecture.md §8.1 ("commencer le prototype en egui pour valider les flux") ; à corriger quand `pdf-app` portera l'état de session partagé entre plusieurs fenêtres/documents.
- **Ouverture de fichier** : dialogue natif via `rfd` (pas de dépendance GTK sur macOS, juste AppKit sous le capot).

## 13. Ce qu'il faut savoir avant de contribuer

- **Chaque limitation est documentée là où elle vit** : en tête de module (`//!`) et dans [STATUS.md](../STATUS.md). Si vous levez une limitation, mettez à jour les deux.
- **Le corpus de test** ([pdf-core/tests/fixtures/](../pdf-core/tests/fixtures/)) est généré par script (reportlab + pikepdf, recette dans son README) — ne modifiez pas les `.pdf` à la main, les tests d'intégration en dépendent octet par octet (`include_bytes!`).
- **CI** : `cargo fmt --check`, `clippy -D warnings`, `cargo test` sur `macos-latest` ([.github/workflows/ci.yml](../.github/workflows/ci.yml)). Le test de substitution système suppose macOS.
- **Convention d'erreur** : jamais de panique sur un fichier malformé ; on dégrade (objet ignoré, reconstruction, placeholder signalé) ou on retourne un `PdfError` précis avec l'offset.

## 14. Carte des fichiers

```
pdf-core/src/
  lexer.rs      tokens depuis octets bruts (positionnable)
  object.rs     modèle COS (Object, Dictionary, Stream, ObjRef)
  parser.rs     objets depuis tokens (lookahead pour N G R, corps de stream)
  xref.rs       xref classique + xref streams + reconstruction par scan
  document.rs   résolution d'objets (offsets + object streams) avec cache
  filters.rs    Flate/LZW/ASCIIHex/ASCII85/DCTDecode + prédicteurs PNG/TIFF
  page.rs       arbre des pages, héritage, concaténation /Contents
  content.rs    tokenisation du flux de contenu (+ saut des images inline)
  interp.rs     exécution des opérateurs → DisplayList
  display.rs    types DisplayList/DisplayItem/Matrix/Color/PathSegment
  encoding.rs   tables WinAnsi/Standard + noms de glyphes AGL
  font.rs       largeurs, Unicode, contours (intégrés TrueType + système macOS)
  image.rs      interprétation /ColorSpace des pixels décodés → RGBA8
pdf-render/src/
  lib.rs        rasterisation tiny-skia (chemins + glyphes + images) → PNG
pdf-cli/src/
  main.rs       dump / render-info / render
pdf-ui/src/
  main.rs       prototype egui/eframe : ouverture, navigation, zoom
```
