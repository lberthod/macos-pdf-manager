# Plan de sprints — macOS PDF Manager

Découpage en sprints à partir de la roadmap par phases décrite dans [architecture.md](./architecture.md#9-roadmap-par-phases-jalons-livrables). Chaque phase de l'architecture est éclatée en sprints de 2 semaines (à ajuster selon la vélocité réelle de l'équipe). Un sprint ne démarre que si le précédent a passé ses critères de sortie.

---

## Sprint 0 — Fondations (Phase 0)

**Objectif :** poser le socle du repo et de l'outillage, sans encore toucher au parsing PDF.

- [x] Créer le workspace Cargo (`pdf-core`, `pdf-text`, `pdf-render`, `pdf-edit`, `pdf-app`, `pdf-ui`, `pdf-cli`) avec crates vides.
- [x] Configurer CI : `cargo fmt --check`, `cargo clippy`, `cargo test` (GitHub Actions).
- [ ] Constituer un premier corpus de PDF de référence (variés : simples, malformés, scannés, formulaires) — un seul fixture minimal existe pour l'instant, corpus large à faire.
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
- [ ] Cross-reference streams (PDF 1.5+) et object streams (`/ObjStm`) — pas encore supportés, nécessaire pour les PDF récents (Acrobat 6+).
- [x] Chaînes de mises à jour incrémentales (`/Prev`).
- [x] Récupération d'erreur : reconstruction par scan `N G obj` si xref corrompue ou `startxref` introuvable.
- [x] Résolution paresseuse des références + cache d'objets.
- [x] Filtres de flux prioritaires : `FlateDecode`, `ASCIIHexDecode`, `ASCII85Decode`. `LZWDecode` et prédicteurs PNG/TIFF restent à faire.

**Critère de sortie (fin Phase 1) :** `pdf-cli dump` affiche la structure de n'importe quel PDF du corpus ; ouverture sans crash sur plusieurs centaines de PDF variés. **Statut réel : validé uniquement sur un fixture minimal fait main — le corpus large et les object/xref streams (PDF 1.5+) manquent encore avant de considérer la Phase 1 terminée.**

---

## Sprint 5-6 — Modèle document & interpréteur de contenu (Phase 2, partie 1)

**Objectif :** exposer une API document/page typée et interpréter le flux de contenu.

- [ ] Modèle document (`Document`, `Page`, catalogue, arbre des pages, ressources).
- [ ] Interpréteur de flux de contenu : état graphique (`q/Q/cm/gs`), chemins (`m l c v y re`, peinture, clipping).
- [ ] Opérateurs texte (`BT/ET`, `Tf`, positionnement, affichage, paramètres).
- [ ] Opérateurs couleur et espaces colorimétriques de base (`DeviceRGB/Gray/CMYK`).
- [ ] Sortie : `DisplayList` (glyphes, chemins, images résolus).

**Critère de sortie :** display list correcte générée pour un sous-ensemble de PDF simples (texte + formes).

---

## Sprint 7-8 — Polices & rendu CPU (Phase 2, partie 2)

**Objectif :** rendre une page à l'écran fidèlement.

- [ ] Polices : TrueType, CFF/Type1C, Type0/CID ; polices intégrées (`/FontFile*`).
- [ ] Substitution système (Core Text) + 14 polices standard.
- [ ] Encodages & CMaps (`/Encoding`, `/ToUnicode`).
- [ ] Rasteriseur CPU via `tiny-skia`.
- [ ] Images : `DCTDecode` (JPEG), `LZWDecode`, `CCITTFaxDecode`.
- [ ] Fenêtre de visualisation prototype (egui).

**Critère de sortie (fin Phase 2) :** rendu pixel-comparé conforme sur le corpus, écart sous le seuil défini par le harnais.

---

## Sprint 9-10 — GPU & UX viewer (Phase 3, partie 1)

**Objectif :** rendu fluide et navigable.

- [ ] Back-end GPU `wgpu` (Metal) : tessellation des chemins (`lyon`), atlas de glyphes.
- [ ] Scroll continu / page à page, zoom molette + pincement trackpad.
- [ ] Cache de rendu par page (tuiles).
- [ ] Recherche texte + sélection (via `pdf-text`).
- [ ] Panneau miniatures, panneau signets/plan.

**Critère de sortie :** navigation fluide sur de gros documents (centaines de pages), recherche fonctionnelle.

---

## Sprint 11-12 — Chrome natif & packaging (Phase 3, partie 2)

**Objectif :** premier produit démontrable.

- [ ] Chrome natif macOS via `objc2`/`cacao` : menus, raccourcis (`⌘S/⌘Z/⌘⇧Z`), glisser-déposer, plein écran, mode sombre.
- [ ] Ouverture/sauvegarde natives (NSOpenPanel/NSSavePanel), Quick Look.
- [ ] Packaging `.dmg` : `cargo-bundle`, signature `codesign`, notarisation `notarytool`.

**Critère de sortie (fin Phase 3) :** `.dmg` signé/notarisé installable, application démontrable en interne. **➜ Jalon : premier produit démontrable.**

---

## Sprint 13-14 — Annotations & formulaires (Phase 4)

**Objectif :** rendre l'éditeur utile au quotidien.

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
