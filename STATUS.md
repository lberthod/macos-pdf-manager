# État précis du projet

**Dernière mise à jour :** 2026-07-04 (substitution de police système ajoutée)
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

**Tests :** 49 tests automatisés (`cargo test --workspace`), tous verts, `cargo clippy --workspace --all-targets` sans avertissement.

---

## 2. Fait, avec limitations explicites

| Domaine | Ce qui marche | Limitation connue |
|---|---|---|
| Xref | Classique + cross-reference streams (PDF 1.5+) + object streams + chaînes `/Prev` | — |
| Récupération d'erreur | Reconstruction par balayage d'octets si xref corrompue/absente, avec repli sur recherche d'un `/Type /Catalog` | Testé sur un seul fichier corrompu artificiellement, pas sur un corpus de corruptions variées |
| Filtres de flux | FlateDecode, ASCIIHex, ASCII85, LZWDecode, prédicteurs PNG (types 0-4) et TIFF | Pas de DCTDecode (JPEG), CCITTFaxDecode, JBIG2Decode, JPXDecode |
| Arbre des pages | Parcours récursif `/Pages`→`/Kids`, héritage Resources/MediaBox/Rotate, garde anti-cycle | — |
| Contenu | ~40 opérateurs : état graphique, chemins, texte, couleur, Form XObjects (récursif, garde de profondeur) | Clip (`W`/`W*`) signalé mais pas appliqué ; patterns/shadings ignorés ; contenu marqué ignoré |
| Police — largeurs | `/Widths`+`/FirstChar` ; repli sur table AFM Helvetica intégrée si absent | Les 13 autres polices standard (Times, Courier...) n'ont pas de table dédiée, retombent sur une largeur par défaut arbitraire (500/1000 em) |
| Police — encodage | `WinAnsiEncoding`/`StandardEncoding` complets (256 codes) + `/Differences` via un sous-ensemble de l'Adobe Glyph List | `/ToUnicode` (CMap dédié, souvent plus précis) n'est pas lu ; `MacRomanEncoding` est approximé par WinAnsi au-delà de l'ASCII |
| Police — contours | Polices **TrueType intégrées** (`/FontFile2`) via `ttf-parser` (repli cmap Macintosh par code brut si pas de cmap Unicode) ; **substitution système macOS** pour les polices standard non intégrées : Helvetica/Times/Courier/Symbol/ZapfDingbats + alias Arial, sélection de la face gras/italique dans les `.ttc`, cache global des fichiers | CFF/Type1C (`/FontFile3`), Type1 (`/FontFile`) : aucun contour. Substitution par lecture directe de `/System/Library/Fonts` (chemins macOS codés en dur, pas via l'API Core Text) — non portable en l'état |
| Rendu | Chemins (fill/stroke/fill+stroke, nonzero/even-odd, courbes de Bézier), glyphes (intégrés **et** substitués), conversion Gray/RGB/CMYK | Pas de rendu des images, pas d'application du clip, pas de GPU |
| Polices composites | Détection de `/Type0` (`Font::is_composite()`) | Aucune gestion réelle : codes 2 octets, `/DescendantFonts`, `/W` CID — tout retombe sur le comportement placeholder (pas d'Unicode, largeur par défaut) |

---

## 3. Ce qui n'existe pas du tout

- **Interface graphique** (`pdf-ui`) : fichier stub vide. Aucun prototype `egui`, aucun chrome natif macOS.
- **Logique applicative** (`pdf-app`) : stub vide. Pas d'état de session, pas de undo/redo.
- **Édition** (`pdf-edit`) : stub vide. Aucune des opérations de la section 7 de architecture.md (pages, annotations, formulaires, texte) n'est implémentée.
- **Extraction de texte structurée** (`pdf-text`) : stub vide (à ce stade, la résolution Unicode des glyphes vit directement dans `pdf-core`, pas encore dans une couche `pdf-text` dédiée à la reconstruction mots/lignes/blocs).
- **Décodage d'images** : JPEG, CCITT, JBIG2, JPX — aucun.
- **Chiffrement PDF** (`/Encrypt`), signatures numériques, PDF/A.
- **Packaging macOS** : pas de `.app`, pas de `.dmg`, pas de signature/notarisation.

---

## 4. Corpus de test

5 fixtures dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) :
- `minimal.pdf` — PDF minimal fait main.
- `multipage_classic_xref.pdf` / `multipage_xref_stream.pdf` — même contenu (5 pages, texte + rectangle), sauvegardé en xref classique et en xref stream + object streams (PDF 1.5+).
- `corrupted_missing_xref.pdf` — fichier tronqué pour tester la reconstruction.
- `embedded_truetype_font.pdf` — police Monaco intégrée en TrueType, pour tester l'extraction de contours réelle.

C'est **loin** du corpus « plusieurs centaines de PDF variés » visé par le critère de sortie de la Phase 1 (architecture.md §9). Aucun PDF scanné, formulaire AcroForm, PDF chiffré, ou document CJK n'a été testé.

---

## 5. Prochaines étapes logiques (par ordre de valeur/effort)

1. **Décodage JPEG (`DCTDecode`)** — nécessaire dès qu'un PDF contient une photo, très fréquent.
2. **Corpus de test large** — condition pour véritablement clore la Phase 1/2 et détecter les régressions sur des cas réels variés.
3. **Prototype UI (`egui`)** — premier pas vers une application utilisable, même minimale.
4. **Contours CFF/Type1C (`/FontFile3`)** — deuxième format de police intégrée le plus courant après TrueType.

Pour le contexte produit (pourquoi ce projet, contraintes, décisions à trancher), voir [architecture.md §1](./architecture.md#1-objectif-et-périmètre) et [§12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage).
