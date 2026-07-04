# Plan de sprints — macOS PDF Manager

Découpage en sprints à partir de la roadmap par phases décrite dans [architecture.md](./architecture.md#9-roadmap-par-phases-jalons-livrables). Chaque phase de l'architecture est éclatée en sprints de 2 semaines (à ajuster selon la vélocité réelle de l'équipe). Un sprint ne démarre que si le précédent a passé ses critères de sortie.

---

## Sprint 0 — Fondations (Phase 0)

**Objectif :** poser le socle du repo et de l'outillage, sans encore toucher au parsing PDF.

- [x] Créer le workspace Cargo (`pdf-core`, `pdf-text`, `pdf-render`, `pdf-edit`, `pdf-app`, `pdf-ui`, `pdf-cli`) avec crates vides.
- [x] Configurer CI : `cargo fmt --check`, `cargo clippy`, `cargo test` (GitHub Actions).
- [ ] Constituer un premier corpus de PDF de référence (variés : simples, malformés, scannés, formulaires) — 4 fixtures existent (`pdf-core/tests/fixtures/`, voir leur README) : classique, xref stream, object streams, corrompu. Corpus large (centaines de PDF, scans, formulaires, chiffrement) toujours à faire.
- [ ] Écrire le harnais de comparaison d'images (diff pixel + seuil) pour les futurs tests de rendu.
- [x] `pdf-cli` minimal (`dump` : ouvre un fichier, affiche sa structure).

**Critère de sortie :** CI verte sur un commit vide, corpus versionné, `pdf-cli` compile et s'exécute.

---

## Sprint 1-2 — Lexer & objets COS (Phase 1, partie 1)

**Objectif :** lire un PDF en tokens puis en objets typés.

- [x] Lexer/tokenizer (`&[u8]` → `Token`), tolérant aux fichiers malformés.
- [x] Modèle `Object` (Null, Boolean, Integer, Real, String, Name, Array, Dictionary, Stream, Reference).
- [x] Parser d'objets indirects (`N G obj ... endobj`).
- [x] Tests unitaires lexer/parser sur cas limites (chaînes échappées, nombres malformés, commentaires).

**Critère de sortie :** parsing correct d'objets isolés sur un jeu de PDF de test.

---

## Sprint 3-4 — Xref & résolution de document (Phase 1, partie 2)

**Objectif :** reconstruire le graphe complet d'un document PDF.

- [x] Table xref classique (`xref`/`trailer`).
- [x] Cross-reference streams (PDF 1.5+) et object streams (`/ObjStm`) — largeurs `/W` variables, `/Index`, entrées type 0/1/2, décodage complet.
- [x] Chaînes de mises à jour incrémentales (`/Prev`).
- [x] Récupération d'erreur : reconstruction par balayage **au niveau des octets** (pas du lexer, pour rester robuste face au contenu binaire des flux compressés) si xref corrompue ou `startxref` introuvable, avec repli sur la recherche d'un `/Type /Catalog` si aucun trailer exploitable n'est trouvé.
- [x] Résolution paresseuse des références + cache d'objets, y compris objets compressés dans un object stream.
- [x] Filtres de flux prioritaires : `FlateDecode`, `ASCIIHexDecode`, `ASCII85Decode`, `LZWDecode`, prédicteurs PNG (0-4) et TIFF.

**Critère de sortie (fin Phase 1) :** `pdf-cli dump` affiche la structure de n'importe quel PDF du corpus ; ouverture sans crash sur plusieurs centaines de PDF variés. **Statut réel : validé sur un corpus de 4 fixtures (xref classique, xref stream + object streams, PDF corrompu récupéré par balayage) générés à partir d'un vrai PDF reportlab/pikepdf — voir `pdf-core/tests/fixtures/README.md`. Le corpus large « plusieurs centaines de PDF variés » (scans, formulaires, PDF chiffrés, CJK, PDF/A) reste à constituer avant de considérer la Phase 1 formellement close.**

---

## Sprint 5-6 — Modèle document & interpréteur de contenu (Phase 2, partie 1)

**Objectif :** exposer une API document/page typée et interpréter le flux de contenu.

- [x] Modèle document (`Document`, `Page`, catalogue, arbre des pages, ressources) — parcours récursif `/Pages` avec héritage `Resources`/`MediaBox`/`Rotate` et garde anti-cycle (`page.rs`).
- [x] Interpréteur de flux de contenu : état graphique (`q/Q/cm`, pile), chemins (`m l c v y re`, peinture `S/s/f/F/f*/B/B*/b/b*/n`), clip (`W/W*`) suivi dans l'état graphique (intersection des clips imbriqués, sauvegarde/restauration par `q`/`Q`) et **appliqué au rendu** (voir Sprint 7-8). `gs` (ExtGState) partiellement pris en compte (`/LW` seulement).
- [x] Opérateurs texte (`BT/ET`, `Tf`, `Td/TD/Tm/T*`, `Tj/TJ/'/"`, `Tc/Tw/Tz/TL/Ts`) — un `DisplayItem::Glyph` par code de caractère brut ; **limitation connue** : ni les codes ne sont résolus en Unicode, ni l'avance ne reflète les vraies largeurs de police (`/Widths`, `/FontFile`) — heuristique constante en attendant Sprint 7-8, signalée via `advance_is_estimated`.
- [x] Opérateurs couleur (`g/G rg/RG k/K sc/scn/SC/SCN`) — espaces colorimétriques déduits du nombre de composantes (1/3/4 = Gray/Rgb/Cmyk) ; `cs/CS` et les espaces nommés (`ICCBased`, `Indexed`, `Separation`) ne sont pas résolus.
- [x] Sortie : `DisplayList` (`display.rs`) — chemins, glyphes (position seulement), images (position seulement, pas de décodage pixel). XObjects Form gérés récursivement (`Do`, avec garde de profondeur) ; XObjects Image et images inline (`BI/ID/EI`) repérés mais pas décodés.

**Critère de sortie :** display list correcte générée pour un sous-ensemble de PDF simples (texte + formes). **Validé** de bout en bout sur les fixtures réels (`pdf-cli render-info`) : un rectangle rempli → 1 `Path`, une ligne de texte → 1 `Glyph` par caractère. Les limitations ci-dessus (largeurs de police, décodage image, clip réel, espaces colorimétriques avancés) restent à lever aux sprints suivants.

---

## Sprint 7-8 — Polices & rendu CPU (Phase 2, partie 2)

**Objectif :** rendre une page à l'écran fidèlement.

- [x] Polices : TrueType intégrée (`/FontFile2`) — extraction de contours réelle via `ttf-parser` (`font.rs::glyph_outline`), avec repli code-brut sur un `cmap` Macintosh (1,0) quand la police n'embarque pas de table Unicode (cas réel rencontré avec un sous-ensemble reportlab/Monaco). **Et** CFF/Type1C intégrée (`/FontFile3`, sous-type `Type1C` via `ttf_parser::cff::Table` directement sur les octets bruts — pas de conteneur OpenType nécessaire — ou sous-type `OpenType` via `ttf_parser::Face`), résolution par l'encodage/charset natif de la table CFF interrogé par code brut. Validé sur fixture réel (police STIX, sous-ensemble 3 glyphes construit à la main avec pikepdf). **Non fait :** Type1 historique (`/FontFile`, pré-CFF), Type0/CID (codes 2 octets, `/DescendantFonts`).
- [x] Substitution système + 14 polices standard — fait par lecture directe des fichiers de `/System/Library/Fonts` (Helvetica/Times/Courier `.ttc` avec sélection de face gras/italique, Symbol, ZapfDingbats, alias Arial→Helvetica, cache global), pas via l'API Core Text (chemins macOS codés en dur, non portable en l'état). Validé visuellement : le fixture Helvetica non intégrée rend son texte réel.
- [x] Encodages & CMaps (`/Encoding`, `/ToUnicode` partiel) — `encoding.rs` : tables `WinAnsiEncoding`/`StandardEncoding` complètes (256 codes) + résolution `/Differences` via un sous-ensemble de l'Adobe Glyph List. `font.rs` combine `/Widths`+`/FirstChar`+`/Encoding` pour produire de vraies largeurs et du texte Unicode réel (validé sur fixture : `"Page 1 - Hello, PDF Manager!"` recomposé exactement). **Non fait :** lecture de `/ToUnicode` (CMap dédié, prioritaire quand présent), polices composites `/Type0`/CID (repli sur l'ancien comportement placeholder), `MacRomanEncoding` dédiée (actuellement approximée par WinAnsi).
- [x] Rasteriseur CPU via `tiny-skia` (`pdf-render`) — dessine les chemins (`fill`/`stroke`/`fill+stroke`, règles nonzero/even-odd, courbes de Bézier) et les glyphes dès qu'un contour a pu être résolu (`DisplayItem::Glyph::outline`, TrueType, CFF/Type1C, ou substitution système), avec conversion Gray/RGB/CMYK→RGB et export PNG. **Clip (`W`/`W*`) réellement appliqué** via un masque `tiny_skia::Mask` rastérisé par intersection des clips imbriqués, mis en cache par chaîne de clip pour éviter de re-rastériser à chaque item (`pdf-render/src/lib.rs::build_clip_mask`). Validé visuellement sur plusieurs fixtures réels (Monaco TrueType, Helvetica substituée, STIX CFF/Type1C) et par test dédié pour le clip. **Non fait :** rendu des glyphes sans contour disponible (Type1 historique, Type0/CID).
- [x] Images : `DCTDecode` (JPEG, via `zune-jpeg`) et `LZWDecode` (fait, voir Phase 1) — décodage complet + interprétation `/ColorSpace` (DeviceGray/RGB/CMYK, ICCBased approximé par `/N`) en RGBA8, **avec canal alpha réel via `/SMask`** (décodé récursivement comme une image DeviceGray, rééchantillonné au plus proche voisin si les dimensions diffèrent), dessinées par `pdf-render` à la bonne position/orientation/transparence (`pdf-core/src/image.rs`, prémultiplication alpha dans `pdf-render` avant `tiny-skia`). Validé sur deux fixtures réels (photo JPEG, image semi-transparente sur rectangle opaque). `CCITTFaxDecode`/`JBIG2Decode`/`JPXDecode` restent à faire ; pas d'espaces `Indexed`/`Separation`, pas de profondeurs autres que 8 bits, pas de `/Mask` (masque de détourage).
- [x] Fenêtre de visualisation prototype (egui) — fait en avance de phase (normalement Sprint 9-10) : `pdf-ui` est un vrai binaire `eframe`/`egui` qui ouvre un fichier (dialogue natif `rfd`), navigue page à page et zoome (re-rasterisation via `pdf_render::render_page_scaled`). Parle désormais à `pdf-app::Session` (état de session : document ouvert, page courante, navigation avec bornes, rendu RGBA agnostique du backend) plutôt que directement à `pdf-core`/`pdf-render` — voir Sprint 9-10 ci-dessous. Validé visuellement par capture d'écran sur un fixture réel.

**Critère de sortie (fin Phase 2) :** rendu pixel-comparé conforme sur le corpus, écart sous le seuil défini par le harnais. **Statut réel : quasiment atteint pour le rendu, avec un premier prototype UI en bonus.** Rendu vectoriel, texte (TrueType/CFF intégrés + substitué système) et images (JPEG, canal alpha `/SMask`) sont fonctionnels et validés visuellement (8 fixtures). Il manque : Type1 historique/Type0-CID, un harnais de comparaison pixel automatisé, et un corpus de test large pour véritablement clore cette phase.

---

## Sprint 9-10 — GPU & UX viewer (Phase 3, partie 1)

**Objectif :** rendu fluide et navigable.

- [ ] Back-end GPU `wgpu` (Metal) : tessellation des chemins (`lyon`), atlas de glyphes.
- [x] Zoom (boutons ＋/－/réinitialiser **et** molette+Ctrl/pincement trackpad via `egui::InputState::zoom_delta`, re-rasterisation) et navigation page suivante/précédente — fait en avance de phase dans le prototype `pdf-ui` (voir Sprint 7-8). **Non fait :** scroll continu entre pages (une page affichée à la fois, avec `ScrollArea` pour le panoramique dans la page zoomée).
- [x] État de session porté par `pdf-app` (`Session::open`/`goto_page`/`next_page`/`prev_page`/`render_current_page`) plutôt que par `pdf-ui` directement — fait en avance de phase, condition posée en note de suivi de Sprint 7-8. `pdf-ui` ne connaît plus `pdf-core`/`pdf-render` que transitivement. **Non fait :** historique undo/redo (prévu Sprint 13-14 avec `EditOp`), multi-documents/onglets.
- [x] Cache de rendu par page — `pdf-app::Session::render_cache` (FIFO, 32 entrées, clé `(page, échelle quantifiée)`) : un second `render_page`/`render_current_page` avec les mêmes paramètres réutilise le `Rc<RenderedPage>` déjà rastérisé sans repasser par `pdf-render`, vérifié par test (`Rc::ptr_eq`). Le **texte** est mis en cache séparément par page (`text_cache`, `Rc<pdf_text::PageText>`) : une deuxième recherche ne ré-interprète aucun flux de contenu déjà vu. **Non fait :** vrai cache de *tuiles* (dalles d'une page à haute résolution) — chaque entrée est une page entière, pertinent seulement pour le futur backend GPU.
- [x] Recherche texte **avec surlignage** — `pdf-text::extract_page_text` garde la position (espace page) de chaque caractère (`PageText::find_matches` renvoie des rectangles fusionnés par occurrence, repli de casse caractère par caractère) ; `pdf-app::Session::find_matches_on_current_page`/`find_pages_containing`/`extract_current_page_text` l'exposent (type `MatchRect` propre à `pdf-app`, pas de dépendance `pdf-text` dans `pdf-ui`) ; `pdf-ui` dessine un surlignage jaune translucide par-dessus la page affichée, recalculé seulement au changement de page/requête. `pdf-cli text` en CLI. **Non fait :** sélection de texte à la souris, reconstruction par blocs/colonnes (`pdftotext -layout`) — les rectangles de surlignage sont approximés (pas de largeur de glyphe réelle, seulement `hauteur_police * 0.6`).
- [x] Panneau miniatures — `pdf-ui` a un panneau latéral (`egui::SidePanel`) listant une miniature cliquable par page (rendue à `THUMBNAIL_SCALE = 0.15` via `Session::render_page`, mise en cache), qui saute directement à la page cliquée. **Non fait :** panneau signets/plan (nécessiterait de lire `/Outlines` depuis le catalogue, pas encore fait côté `pdf-core`).

**Critère de sortie :** navigation fluide sur de gros documents (centaines de pages), recherche fonctionnelle. **Statut réel :** recherche fonctionnelle avec surlignage, miniatures et cache de rendu par page faits ; il reste le back-end GPU `wgpu` et le scroll continu entre pages pour clore complètement ce sprint.

---

## Sprint 11-12 — Chrome natif & packaging (Phase 3, partie 2)

**Objectif :** premier produit démontrable.

- [ ] Chrome natif macOS via `objc2` : menus, raccourcis (`⌘S/⌘Z/⌘⇧Z`), glisser-déposer, plein écran, mode sombre.
- [ ] Ouverture/sauvegarde natives (NSOpenPanel/NSSavePanel), Quick Look.
- [ ] Packaging `.dmg` : `cargo-bundle`, signature `codesign`, notarisation `notarytool`.

**Décision de dépendance :** `objc2` + crates officielles par framework (`objc2-foundation`, `objc2-app-kit`, `objc2-quartz-core`) plutôt que `cacao`, pour éviter de mélanger plusieurs couches de bindings Objective-C dans la même application.

**Critère de sortie (fin Phase 3) :** `.dmg` signé/notarisé installable, application démontrable en interne. **➜ Jalon : premier produit démontrable.**

---

## Sprint 13-14 — Annotations & formulaires (Phase 4)

**Objectif :** rendre l'éditeur utile au quotidien.

**Prérequis technique (pas urgent, à faire avant ce sprint) :** migrer `Object::Array(Vec<Object>)`/`Object::Dictionary(Dictionary)` vers des types partagés (`Arc<[Object]>`, `Arc<Dictionary>`) et ajouter un `SourceSpan` (offset début/fin dans le fichier source) à chaque objet indirect. Utile pour la sauvegarde incrémentale, l'édition structurelle et le débogage — mais à ne déclencher qu'au moment d'attaquer ce sprint, pas préventivement.

- [ ] Annotations : surlignage, notes, formes, texte libre, signatures.
- [ ] Remplissage de formulaires AcroForm.
- [ ] Journal d'opérations (`EditOp`) + undo/redo.
- [ ] Sauvegarde incrémentale (append xref).

**Critère de sortie :** annotations et remplissage de formulaires persistés correctement après réouverture du fichier. **➜ Jalon : éditeur utile pour l'usage courant.**

---

## Sprint 15-16 — Manipulation de pages (Phase 5)

**Objectif :** opérations documentaires de haut niveau.

- [ ] Insérer / supprimer / déplacer / pivoter des pages.
- [ ] Fusion et découpage de documents.
- [ ] Insertion d'images et de pages depuis d'autres PDF.
- [ ] Export / optimisation (linéarisation, garbage collection des objets orphelins).

**Critère de sortie :** toutes les opérations de pages validées sur le corpus, sans corruption de document.

---

## Sprint 17+ — Édition de texte (Phase 6, périmètre progressif)

**Objectif :** livrer une édition de texte réaliste par étapes, en assumant les limites documentées en [architecture.md §7.3](./architecture.md#73-édition-du-texte-existant--le-vrai-défi).

- [ ] **6a.** Ajout de nouveau texte (annotations FreeText gérées par l'éditeur).
- [ ] **6b.** Remplacement par superposition d'un texte existant (masquer l'ancien + redessiner).
- [ ] **6c. (R&D, long terme)** Édition chirurgicale du flux de contenu + gestion des subsets de polices, limitée aux PDF bien formés.

**Portée des crates de shaping (`rustybuzz`/`swash`/`cosmic-text`, si utilisées) :** strictement réservées au layout du **nouveau texte ajouté par l'utilisateur** (6a/6b). Ne jamais les utiliser pour réinterpréter le texte existant du PDF : ses positions et glyphes viennent déjà du flux de contenu original et sont gérés par `pdf-core::font.rs`/`ttf-parser`. Un relayout automatique du contenu d'origine serait incorrect.

**Critère de sortie :** 6a/6b livrés et testés avant d'engager du temps sur 6c ; 6c traité comme un projet de recherche séparé, budgété à part.

---

## Sprint 18+ — Durcissement (Phase 7)

**Objectif :** fiabiliser avant diffusion plus large.

- [ ] Fuzzing du parser (`cargo-fuzz`).
- [ ] Optimisation performance sur gros fichiers.
- [ ] Accessibilité, conformité PDF/A.
- [ ] Chiffrement (`/Encrypt`, RC4/AES).
- [ ] Signatures numériques.

---

## Notes de suivi

- Les sprints 1 à 12 (Phases 0-3) doivent produire un viewer complet avant tout travail d'édition — voir l'avertissement en tête de [architecture.md](./architecture.md#1-objectif-et-périmètre).
- Les points de décision de [architecture.md §12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage) doivent être tranchés avant le Sprint 0 (frontière maison/crates, framework GUI, périmètre édition, cibles de compatibilité, budget, distribution).
- Réévaluer la durée des sprints après le Sprint 4 (premier retour de vélocité réelle sur le parsing, généralement la partie la plus imprévisible).
