# Éditeur / Visionneuse PDF natif macOS en Rust — Document d'architecture

**Version :** 1.0
**Date :** 2026-07-03
**Auteur :** Loïc (spécification) — à transmettre à l'équipe de développement
**Public visé :** développeur(euse) Rust chargé(e) de l'implémentation

---

## 1. Objectif et périmètre

Construire une application **native macOS** (livrée en `.dmg`) permettant de :

1. **Visualiser** des PDF (rendu fidèle, zoom, navigation, recherche texte).
2. **Annoter** (surlignage, notes, formes, signatures, remplissage de formulaires).
3. **Manipuler les pages** (réorganiser, supprimer, pivoter, fusionner, découper, insérer).
4. **Éditer le contenu** — y compris **modifier le texte** existant et les objets vectoriels.

Deux contraintes fortes fixées par le commanditaire :

- **Rust natif**, distribution `.dmg` macOS.
- **Moteur PDF « maison »** : le parsing et le rendu sont écrits *from scratch* en Rust, sans dépendre d'un moteur tiers (pdfium, MuPDF…).

> ⚠️ **Avertissement d'ingénierie (à lire avant de chiffrer).**
> L'édition **complète du texte** d'un PDF avec un **moteur écrit de zéro** est l'un des projets les plus difficiles de l'écosystème documentaire. Un PDF n'est pas un format « éditable » : c'est une description de rendu (des glyphes positionnés à des coordonnées absolues, sans notion de paragraphe, de ligne, ni parfois d'espace). Éditer du texte suppose de reconstruire une couche sémantique qui n'existe pas dans le fichier. Adobe, Foxit et MuPDF y consacrent des équipes depuis 20+ ans.
>
> Ce document décrit l'architecture cible complète, **mais recommande une exécution par phases** (section 9). La phase « édition de texte » doit être abordée après un socle viewer + annotations solide, et son périmètre réaliste est détaillé en section 7. Prévoir un budget pluriannuel pour atteindre le niveau « édition complète » sur des PDF quelconques.

---

## 2. Principes directeurs

- **Séparation stricte cœur / interface.** Le moteur PDF (`pdf-core`) ne connaît rien de l'UI. Il est testable en isolation, réutilisable (CLI, tests, futur portage).
- **Modèle document immuable + journal d'édits.** Le PDF chargé n'est jamais muté en place ; on maintient un *document model* et une pile d'opérations (undo/redo, sauvegarde incrémentale).
- **Rendu fidèle avant tout.** Un viewer qui affiche mal disqualifie l'éditeur. Le rendu est la priorité n°1 et le principal juge de la qualité du moteur maison.
- **Sécurité mémoire = argument clé du choix Rust.** Le parsing de formats binaires hostiles (PDF est un vecteur d'attaque classique) bénéficie directement des garanties Rust. Zéro `unsafe` dans le parser sauf justification documentée.
- **Incrémental et observable.** Chaque couche (lexer → objets → pages → contenu → rendu) est livrable et testable indépendamment.

---

## 3. Vue d'ensemble de l'architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Application macOS (.app)                  │
│                                                               │
│  ┌────────────────────────────────────────────────────────┐  │
│  │  UI Layer  (GUI framework Rust)                         │  │
│  │  - Fenêtre, menus macOS, barre d'outils                 │  │
│  │  - Vue document (scroll, zoom, sélection)              │  │
│  │  - Panneaux : miniatures, annotations, propriétés      │  │
│  └───────────────┬────────────────────────────────────────┘  │
│                  │ commandes / événements                     │
│  ┌───────────────▼────────────────────────────────────────┐  │
│  │  App / Controller Layer  (pdf-app)                     │  │
│  │  - État de session, document ouvert(s)                 │  │
│  │  - Pile undo/redo, journal d'édits                     │  │
│  │  - Orchestration rendu ↔ édition                       │  │
│  └───────────────┬────────────────────────────────────────┘  │
│                  │                                            │
│  ┌───────────────▼────────────────────────────────────────┐  │
│  │  Moteur PDF maison  (pdf-core)  — cœur Rust pur         │  │
│  │                                                        │  │
│  │  parser  ─►  model  ─►  content  ─►  render  ─►  write  │  │
│  │  (lexer,     (objets,   (flux de     (raster/    (save,  │  │
│  │   xref,       pages,     contenu,     vector      linéa- │  │
│  │   objets)     arbre)     opérateurs)  tessell.)   risé)  │  │
│  │                                                        │  │
│  │  text  │  fonts  │  images  │  annots  │  forms          │  │
│  └────────────────────────────────────────────────────────┘  │
│                                                               │
│  Dépendances système : Core Text / Core Graphics (fonts,      │
│  éventuellement rendu accéléré), Metal/wgpu (GPU)             │
└─────────────────────────────────────────────────────────────┘
```

Découpage en **crates d'un workspace Cargo** :

| Crate | Rôle | Dépend de |
|---|---|---|
| `pdf-core` | Moteur : parsing, modèle, contenu, rendu, écriture | — (Rust pur + libs bas niveau) |
| `pdf-text` | Extraction / analyse / réécriture de la couche texte | `pdf-core` |
| `pdf-render` | Rasterisation & rendu vectoriel (CPU + GPU) | `pdf-core` |
| `pdf-edit` | Opérations d'édition, journal, undo/redo | `pdf-core`, `pdf-text` |
| `pdf-app` | Logique applicative, état, contrôleur | `pdf-*` |
| `pdf-ui` | Interface graphique macOS | `pdf-app` |
| `pdf-cli` | Outil ligne de commande (debug, tests, batch) | `pdf-core`, `pdf-render` |

> Séparer `pdf-render` de `pdf-core` permet de tester le parsing sans le rendu, et d'itérer sur le rendu (CPU→GPU) sans toucher au parser.

---

## 4. Le moteur PDF maison (`pdf-core`)

Le moteur suit la spécification **ISO 32000** (PDF 1.7, alignée sur l'ex-spec Adobe, disponible librement ; PDF 2.0 / ISO 32000-2 en cible ultérieure). Il se construit en **couches empilées**, chacune consommant la précédente.

### 4.1 Couche 1 — Lexer / Tokenizer

Découpe le flux d'octets en tokens PDF : nombres, chaînes littérales `(...)` et hexadécimales `<...>`, noms `/Name`, mots-clés (`obj`, `endobj`, `stream`, `R`, `true`…), délimiteurs `[] << >>`.

- Entrée : `&[u8]` (jamais `&str` — le PDF est binaire, encodages multiples).
- Robustesse : tolérer les fichiers malformés (très courants dans la nature). Un lexer trop strict rejette la moitié du web.
- Sortie : itérateur de `Token`.

### 4.2 Couche 2 — Parser d'objets & table xref

Reconstruit le graphe d'objets PDF (le *COS model* : Carousel Object System).

Types d'objets à modéliser :

```rust
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),          // littérale ou hex, décodée en octets
    Name(Name),               // /Type, /Font, …
    Array(Vec<Object>),
    Dictionary(Dictionary),   // map Name -> Object
    Stream(Stream),           // Dictionary + données brutes
    Reference(ObjRef),        // « 12 0 R »
}

pub struct ObjRef { pub num: u32, pub gen: u16 }
```

Responsabilités clés :

- **Table de références croisées (xref)** : classique (`xref`/`trailer`) **et** *cross-reference streams* (PDF 1.5+). Gérer les **object streams** (`/Type /ObjStm`) qui compressent plusieurs objets.
- **Chaînes de mises à jour incrémentales** : un PDF peut contenir plusieurs sections xref empilées (`/Prev`). Résoudre la version la plus récente de chaque objet.
- **Récupération d'erreur** : si la xref est corrompue, **reconstruire** en scannant le fichier à la recherche des `N G obj` (fallback indispensable, comme le font tous les lecteurs réels).
- **Résolution paresseuse** des références (`ObjRef` → `Object`) avec cache, pour ne pas tout charger en mémoire.

### 4.3 Couche 3 — Filtres de flux (stream decoding)

Les `stream` sont encodés par une chaîne de filtres. À implémenter :

| Filtre | Priorité | Note |
|---|---|---|
| `FlateDecode` (zlib/deflate) | **Critique** | ~90 % des flux. Crate `flate2` acceptable (ce n'est pas le « moteur PDF »). |
| `ASCIIHexDecode`, `ASCII85Decode` | Haute | Simples, maison. |
| `LZWDecode` | Moyenne | Ancien mais présent. |
| `DCTDecode` (JPEG) | Haute | Images. Crate `jpeg-decoder` / `zune-jpeg`. |
| `CCITTFaxDecode` | Moyenne | Fax / scans N&B. |
| `JBIG2Decode`, `JPXDecode` (JPEG2000) | Basse | Rares, complexes — reporter. |
| Prédicteurs PNG/TIFF | Haute | Utilisés avec Flate sur les xref streams. |

> **Décision assumée :** « moteur maison » concerne la *logique PDF* (structure, contenu, rendu). S'appuyer sur des crates éprouvées pour les **codecs génériques** (deflate, JPEG) est raisonnable et n'enlève rien au caractère maison du moteur — réécrire zlib serait du gaspillage. À valider avec le commanditaire ; ce document le recommande explicitement.

### 4.4 Couche 4 — Modèle document (arbre logique)

Au-dessus du graphe d'objets brut, une API typée et ergonomique :

- **Catalogue** (`/Root`) → **arbre des pages** (`/Pages`, nœuds `/Page`).
- **Page** : `MediaBox`/`CropBox`, `Rotate`, ressources (`/Resources` : fonts, images, color spaces, ExtGState), flux de contenu (`/Contents`), annotations (`/Annots`).
- **Métadonnées** (`/Info`, XMP), signets (`/Outlines`), liens, destinations.
- **Formulaires AcroForm** (`/AcroForm`, champs).

```rust
pub struct Document { /* objets, xref, catalogue */ }
impl Document {
    pub fn open(bytes: Vec<u8>) -> Result<Self, PdfError>;
    pub fn page_count(&self) -> usize;
    pub fn page(&self, index: usize) -> Result<Page, PdfError>;
    pub fn metadata(&self) -> Metadata;
}
```

### 4.5 Couche 5 — Interpréteur de flux de contenu

Le flux de contenu d'une page est un mini-langage de ~70 opérateurs (notation postfixée). L'interpréteur maintient un **graphics state** (matrice CTM, couleurs, épaisseur de trait, état texte) et une pile.

Familles d'opérateurs à gérer :

- **État graphique** : `q`/`Q` (save/restore), `cm` (transformation), `gs`.
- **Chemins** : `m l c v y re` (construction), `S s f F B b n` (peinture), `W` (clipping).
- **Texte** : `BT`/`ET`, `Tf` (font+taille), `Td TD Tm T*` (positionnement), `Tj TJ ' "` (affichage), `Tc Tw Tz TL Ts Tr` (paramètres).
- **Couleur** : `g rg k cs sc scn` + espaces colorimétriques (`DeviceRGB/Gray/CMYK`, `ICCBased`, `Indexed`, `Separation`).
- **Images & XObjects** : `Do` (form & image), `BI/ID/EI` (inline images).
- **Marquage** : `BMC/BDC/EMC` (contenu marqué — utile pour l'accessibilité et l'extraction structurée).

Sortie de l'interpréteur : une **liste d'affichage** (*display list*) — séquence de primitives résolues (glyphes positionnés, chemins remplis/tracés, images placées) indépendante des détails du langage. C'est cette display list que consomment le renderer **et** l'extracteur de texte.

### 4.6 Couche 6 — Polices (`fonts`)

Le point le plus délicat après le parsing. Types à gérer :

- **Type1**, **TrueType**, **CFF/Type1C**, **Type0/CID** (composites, CJK), **Type3** (glyphes définis par des flux de contenu).
- **Polices intégrées** (`/FontFile`, `/FontFile2`, `/FontFile3`) : extraire et rasteriser les glyphes.
- **Polices non intégrées** : substitution via les polices système (Core Text sur macOS) + les 14 polices standard.
- **Encodages & CMaps** : mapping code → glyphe → Unicode (`/Encoding`, `/ToUnicode`) — indispensable pour la recherche, la sélection et l'extraction de texte correctes.

Recommandation : s'appuyer sur des crates de *font shaping/rasterization* matures (`ttf-parser`, `rustybuzz`, `swash`, `cosmic-text`) pour transformer les glyphes en contours/bitmaps. Le moteur maison gère la **logique PDF des polices** (résolution, encodage, CMap, métriques) ; la rasterisation de glyphes est un problème générique déjà bien résolu en Rust.

---

## 5. Rendu (`pdf-render`)

Deux back-ends, même display list en entrée :

1. **CPU (référence)** : rasteriseur logiciel via `tiny-skia` (chemins, remplissage, anti-aliasing conformes à la sémantique PDF/PostScript). Simple, déterministe, parfait pour les tests de non-régression *pixel-perfect*.
2. **GPU (performance)** : via `wgpu` (Metal sous le capot sur macOS) pour le zoom fluide et les gros documents. Cible : tessellation des chemins (`lyon`), atlas de glyphes, compositing.

Stratégie : **livrer d'abord le back-end CPU** (correction avant vitesse), puis introduire le GPU une fois le rendu validé.

Points d'attention rendu :

- Espaces colorimétriques et blend modes (transparence PDF, groupes de transparence — complexe).
- Clipping arbitraire, patterns, shadings (dégradés `sh`).
- Cache de rendu par page (tuiles) pour le scroll.
- Rendu à la demande hors écran + résolution adaptée au zoom (éviter de rasteriser à 100 % puis upscaler).

**Tests de rendu** : constituer un corpus de PDF de référence, générer des images, comparer par différence de pixels avec un seuil. C'est le principal garde-fou qualité du moteur.

---

## 6. Extraction & couche texte (`pdf-text`)

Depuis la display list :

- **Extraction** : regrouper les glyphes en mots/lignes/blocs par heuristiques géométriques (proximité, ligne de base, ordre de lecture). Le PDF ne stocke ni mots ni lignes — tout est reconstruit.
- **Recherche** : index Unicode via les tables `ToUnicode`, insensible à la casse, tolérant aux ligatures.
- **Sélection** : mapper une sélection écran → suite de glyphes → texte Unicode.
- **Export** : texte brut, éventuellement structuré (via contenu marqué).

Cette couche est le **socle de l'édition de texte** (section 7) : on ne peut éditer que ce qu'on sait localiser et interpréter.

---

## 7. Édition (`pdf-edit`) — approche réaliste

### 7.1 Modèle d'édition

- Le document ouvert reste **immuable**. Les modifications sont des **opérations** appliquées à une couche d'édition superposée.
- **Journal d'opérations** → undo/redo gratuit + sauvegarde **incrémentale** (append d'une nouvelle section xref, sans réécrire tout le fichier — natif au format PDF et sûr).
- Sauvegarde « complète » (linéarisation / *garbage collection* des objets orphelins) en option d'export.

```rust
pub enum EditOp {
    // Niveau page (robuste, à faire en premier)
    InsertPage { at: usize, source: PageSource },
    DeletePage { index: usize },
    MovePage   { from: usize, to: usize },
    RotatePage { index: usize, degrees: i32 },
    // Annotations (robuste)
    AddAnnotation { page: usize, annot: Annotation },
    EditAnnotation { id: AnnotId, change: AnnotChange },
    RemoveAnnotation { id: AnnotId },
    // Formulaires
    SetFormField { field: FieldId, value: FieldValue },
    // Contenu (difficile)
    InsertText { page: usize, at: Point, run: TextRun },
    EditTextRun { id: TextRunId, new_text: String },
    DeleteObject { id: ContentObjId },
    // …
}
```

### 7.2 Édition de pages, annotations, formulaires — **faisable**

Ces opérations manipulent la **structure** (arbre de pages, dictionnaires d'annotations, AcroForm) sans réécrire le contenu graphique. C'est robuste, bien spécifié, et constitue 80 % de la valeur perçue d'un « éditeur PDF » grand public. **À livrer en premier.**

### 7.3 Édition du **texte existant** — le vrai défi

Modifier « ceci » en « cela » dans un PDF implique, dans le cas général :

1. Localiser les glyphes concernés dans le flux de contenu (via `pdf-text`).
2. Reconstruire une notion de ligne/paragraphe **absente du fichier**.
3. Re-*shaper* le nouveau texte avec la **même police** — qui peut être **partiellement intégrée** (*subset* : seuls les glyphes utilisés sont présents ; le caractère qu'on veut taper peut ne pas exister dans la police du fichier).
4. Recalculer positions, justification, retour à la ligne, et **réécrire le flux de contenu** de la page.
5. Gérer les effets de bord : texte qui déborde, chevauche, décale la mise en page.

Conséquences pour le périmètre réaliste :

- **Édition « in-place » simple** (corriger un mot, une police intégrée complète, ligne isolée) : atteignable avec un effort important.
- **Ré-agencement de paragraphe / reflow** : très difficile, résultats imparfaits même chez les leaders du marché.
- **Cas « police subset sans le glyphe voulu »** : nécessite d'**embarquer/étendre** la police (fusion de subsets, ré-encodage) — sous-projet à part entière.

**Recommandation de périmètre pour la V1 d'édition texte :**
- Ajouter du **nouveau** texte (zones de texte / annotations *FreeText*) : facile, à faire tôt.
- **Éditer** une portée de texte existante en la **remplaçant par un objet texte géré par l'éditeur** (on masque l'ancien, on redessine) plutôt que de muter chirurgicalement le flux d'origine : bien plus robuste, résultat visuel équivalent pour l'utilisateur.
- Réserver l'édition « chirurgicale » du flux de contenu à une phase ultérieure, sur un sous-ensemble de PDF « bien formés ».

---

## 8. Interface macOS (`pdf-ui`)

### 8.1 Choix du framework GUI

| Option | Pour | Contre | Verdict |
|---|---|---|---|
| **egui** (immediate mode) | Rapide à développer, 100 % Rust, portable, bon pour outils/inspecteurs | Look non-natif macOS, gestion texte/IME perfectible | Bon pour **prototype** et panneaux techniques |
| **Iced** (Elm-like, retained) | Architecture propre, 100 % Rust, wgpu | Écosystème jeune, intégration macOS partielle | Candidat sérieux V1 |
| **Slint** | Déclaratif, perf, tooling correct | Licence à vérifier selon distribution | À évaluer |
| **Tauri** (webview UI) | UI web riche, menus natifs faciles | UI non-Rust (HTML/JS), moins « natif » | Écarté (contrainte « natif ») |
| **AppKit direct** via `objc2`/`cacao` | Vrai natif macOS (menus, IME, services, plein écran) | Verbeux, `unsafe` FFI, courbe raide | Pour le **chrome natif** (fenêtre, menus, `.dmg`) |

**Recommandation :** UI de rendu du document en **wgpu** (partagé avec `pdf-render`), enrobée d'un **chrome natif AppKit** via `objc2`/`cacao` pour l'intégration macOS (barre de menus, raccourcis, glisser-déposer, IME pour la saisie texte, sauvegarde/ouverture natives). Commencer le prototype en **egui** pour valider les flux, migrer le chrome vers natif ensuite. Décision à arbitrer explicitement avec le dev en début de projet (voir section 12).

### 8.2 Composants UI

- Vue document : scroll continu / page à page, zoom (molette + pincement trackpad), rotation.
- Panneau **miniatures** (drag-and-drop pour réorganiser les pages).
- Panneau **annotations** / commentaires.
- **Palette d'outils** : sélection, surligneur, note, formes, texte, signature, tampon.
- **Inspecteur de propriétés** (objet sélectionné, page, métadonnées).
- **Recherche** (surlignage des occurrences, navigation).
- Panneau **signets / plan**.
- Intégration macOS : menus standard, `⌘S/⌘Z/⌘⇧Z`, plein écran, mode sombre, Services, Quick Look.

---

## 9. Roadmap par phases (jalons livrables)

> Chaque phase est un incrément **utilisable et testable**. Ne pas commencer une phase avant que la précédente passe ses tests de référence.

**Phase 0 — Fondations (workspace, CI, corpus de tests)**
Workspace Cargo, crates vides, CI (fmt, clippy, tests), corpus de PDF de référence + harnais de comparaison d'images. `pdf-cli` minimal.

**Phase 1 — Parser (le socle)**
Lexer, objets COS, xref (classique + streams + object streams), mises à jour incrémentales, récupération d'erreur, filtres Flate/ASCII/LZW. Livrable : `pdf-cli dump` qui affiche la structure de n'importe quel PDF. *Critère de sortie : ouvrir sans crash un corpus varié de plusieurs centaines de PDF.*

**Phase 2 — Rendu (viewer)**
Interpréteur de contenu → display list, polices (intégrées + substitution système), rasteriseur CPU (`tiny-skia`), images (JPEG/Flate). Fenêtre de visualisation (prototype egui). *Critère : rendu pixel-comparé conforme sur le corpus, écart < seuil.*

**Phase 3 — Viewer complet & UX macOS**
GPU (wgpu), scroll/zoom fluides, recherche & sélection texte, miniatures, signets, chrome natif macOS, packaging `.dmg` signé/notarisé. **➜ Premier produit démontrable.**

**Phase 4 — Annotations & formulaires**
Surlignage, notes, formes, texte libre, signatures ; remplissage AcroForm ; journal undo/redo ; sauvegarde incrémentale. **➜ « Éditeur » utile pour l'usage courant.**

**Phase 5 — Manipulation de pages**
Insérer/supprimer/déplacer/pivoter, fusion/split, insertion d'images et de pages depuis d'autres PDF. Export/optimisation.

**Phase 6 — Édition de texte (périmètre progressif)**
6a. Ajout de texte (FreeText géré par l'éditeur).
6b. « Remplacement par superposition » d'un texte existant (masquer + redessiner).
6c. (Long terme, R&D) Édition chirurgicale du flux de contenu + gestion des subsets de polices, sur PDF bien formés.

**Phase 7 — Durcissement**
Fuzzing du parser (`cargo-fuzz`), performance sur gros fichiers, accessibilité, PDF/A, chiffrement (`/Encrypt`, RC4/AES), signatures numériques.

---

## 10. Choix techniques recommandés (récapitulatif)

| Domaine | Recommandation | Alternative |
|---|---|---|
| Langage | Rust stable, édition 2021+ | — |
| Structure | Workspace Cargo multi-crates | — |
| Deflate/zlib | `flate2` (backend `miniz_oxide`, pur Rust) | — |
| JPEG | `zune-jpeg` / `jpeg-decoder` | — |
| Rasterisation CPU | `tiny-skia` | maison |
| Rendu GPU | `wgpu` (→ Metal) + `lyon` (tessellation) | — |
| Parsing/rasterisation de glyphes | `ttf-parser`, `swash`, `cosmic-text`, `rustybuzz` | Core Text (FFI) |
| GUI prototype | `egui` | `iced` |
| Chrome natif macOS | `objc2` / `cacao` | AppKit brut |
| Packaging | `cargo-bundle` / scripts `codesign` + `notarytool` | `.app` manuel |
| Tests binaires hostiles | `cargo-fuzz`, `proptest` | — |
| Sérialisation debug | `serde` (rapports JSON) | — |

> La règle d'arbitrage « maison vs crate » : **le moteur maison, c'est la logique PDF** (structure, contenu, polices au sens PDF, rendu sémantique, écriture). Les **codecs génériques et la rasterisation de bas niveau** (deflate, JPEG, contours de glyphes) s'appuient sur des crates éprouvées. Ce choix doit être confirmé explicitement au démarrage.

---

## 11. Modèle de données — esquisse

```rust
// pdf-core
pub struct Document {
    raw: Arc<[u8]>,              // fichier d'origine, immuable
    xref: XrefTable,            // num -> offset / objstm
    trailer: Dictionary,
    cache: ObjectCache,         // résolution paresseuse
}

pub struct Page<'d> {
    doc: &'d Document,
    dict: Dictionary,
    media_box: Rect,
    rotate: i32,
    resources: Resources,
    contents: Vec<StreamRef>,
    annots: Vec<Annotation>,
}

// Résultat de l'interprétation du contenu
pub struct DisplayList {
    items: Vec<DisplayItem>,
}
pub enum DisplayItem {
    Glyph { font: FontId, gid: u16, unicode: Option<char>,
            transform: Matrix, fill: Paint },
    Path  { path: PathData, paint: PaintOp, clip: Option<ClipId> },
    Image { image: ImageId, transform: Matrix },
}

// pdf-edit : couche superposée
pub struct EditSession {
    base: Arc<Document>,
    ops: Vec<EditOp>,           // journal ordonné
    undo_cursor: usize,
}
impl EditSession {
    pub fn apply(&mut self, op: EditOp);
    pub fn undo(&mut self);
    pub fn redo(&mut self);
    pub fn save_incremental(&self, out: &mut impl Write) -> Result<()>;
    pub fn export_flattened(&self, out: &mut impl Write) -> Result<()>;
}
```

---

## 12. Points à trancher avec le développeur (avant le démarrage)

1. **Confirmer la frontière « maison vs crates »** (section 10). Réécrire deflate/JPEG/rasterisation de glyphes n'apporte rien et alourdit énormément le projet.
2. **Framework GUI définitif** (egui prototype → natif ? Iced ? Slint ?). Impacte le budget UI.
3. **Périmètre exact de l'« édition complète »** (section 7.3) : accepter l'approche « superposition » en V1 vs viser l'édition chirurgicale.
4. **Cibles de compatibilité** : PDF 1.7 seul, ou aussi 2.0 ? PDF chiffrés ? PDF/A ? CJK ?
5. **Budget & équipe** : le périmètre « moteur maison + édition texte complète » est réaliste sur **plusieurs années** à temps plein. Prioriser les phases 1–5 pour un produit livrable, traiter la phase 6c en R&D.
6. **Distribution** : compte développeur Apple pour signature + notarisation du `.dmg` (obligatoire pour une diffusion hors App Store sans alertes Gatekeeper).

---

## 13. Risques principaux

| Risque | Impact | Mitigation |
|---|---|---|
| Sous-estimation de l'édition texte | Élevé | Périmètre progressif (7.3), approche superposition d'abord |
| Diversité des polices intégrées | Élevé | S'appuyer sur crates de fonts matures, corpus de test large |
| PDF malformés dans la nature | Moyen | Récupération d'erreur, fuzzing, tolérance du lexer |
| Fidélité de rendu (transparence, shadings) | Moyen | Back-end CPU de référence + tests pixel |
| Complexité GUI native macOS | Moyen | Prototype egui, chrome natif isolé et incrémental |
| Sécurité (parsing hostile) | Moyen | Rust safe, zéro `unsafe` non justifié, `cargo-fuzz` |

---

## 14. Références normatives

- **ISO 32000-1:2008** (PDF 1.7) — spécification de référence (l'ex-« PDF Reference 1.7 » d'Adobe est librement disponible et équivalente).
- **ISO 32000-2** (PDF 2.0) — cible ultérieure.
- Spécifications de polices : *OpenType*, *TrueType*, *CFF/Type2*, *Adobe Type 1*.
- Écosystème Rust de référence pour s'inspirer (sans en dépendre) : `pdf`, `lopdf`, `pdf-rs`, `printpdf`, `pdfium-render` (wrapper), `mupdf-rs`.

---

*Fin du document. Ce document décrit une cible d'architecture complète et une trajectoire d'exécution réaliste. Il est conçu pour être remis tel quel à un développeur Rust, discuté en section 12, puis affiné en spécifications détaillées phase par phase.*
