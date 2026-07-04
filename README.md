# macOS PDF Manager

Éditeur / visionneuse PDF **natif macOS**, écrit en **Rust**, distribué en `.dmg`, avec un **moteur PDF maison** (parsing et rendu écrits from scratch, sans dépendre de pdfium/MuPDF).

## Fonctionnalités visées

1. **Visualiser** des PDF (rendu fidèle, zoom, navigation, recherche texte).
2. **Annoter** (surlignage, notes, formes, signatures, remplissage de formulaires).
3. **Manipuler les pages** (réorganiser, supprimer, pivoter, fusionner, découper, insérer).
4. **Éditer le contenu**, y compris le texte existant et les objets vectoriels (périmètre progressif, voir plus bas).

> ⚠️ L'édition complète du texte d'un PDF avec un moteur écrit de zéro est un projet difficile : un PDF est une description de rendu (glyphes positionnés), pas un format éditable. Le périmètre réaliste et l'approche par phases sont détaillés dans [architecture.md](./architecture.md).

## État actuel (voir [STATUS.md](./STATUS.md) pour le détail précis)

Le projet a un **moteur PDF fonctionnel de bout en bout** sur un sous-ensemble réel de PDF : ouverture d'un fichier → xref (classique et streams PDF 1.5+) → arbre des pages → interprétation du flux de contenu (chemins, texte, couleur, clip, Form XObjects) → rendu **CPU** (`tiny-skia`) **et GPU** (`wgpu`, en parité fonctionnelle) en PNG ou à l'écran, avec de vraies métriques de police et de vrais contours de glyphes — polices TrueType et CFF/Type1C **intégrées** (simples **et composites** `/Type0`/CID, `CIDFontType0` et `CIDFontType2`), polices standard **non intégrées** (substituées par les polices système macOS : Helvetica, Times, Courier..., gras/italique), images JPEG (RGB et CMYK) avec canal alpha (`/SMask`).

Un premier **prototype de viewer graphique** (`pdf-ui`, `egui`/`eframe`) est fonctionnel : navigation, zoom, recherche texte avec surlignage, miniatures, panneau de signets, défilement continu, sélection de texte à la souris.

L'application a désormais un vrai **chrome natif macOS** : barre de menus système (`NSMenu` via `objc2`/`objc2-app-kit`, pas dessinée par `egui`), ouverture/export de fichier natifs, glisser-déposer, plein écran, mode sombre — packagée en `.app`/`.dmg` via `cargo-bundle`/`hdiutil`.

Ce qui **ne fonctionne pas encore** : l'édition, l'annotation, la manipulation de pages, Quick Look, la signature/notarisation Apple réelle (identifiants Developer non disponibles dans cet environnement), Type1 historique (police pré-CFF), les images CCITT/JBIG2/JPX, le déchiffrement PDF (un PDF chiffré est détecté proprement mais jamais lu). Voir [STATUS.md](./STATUS.md) pour la liste précise, fichier par fichier, de ce qui est fait et de ce qui manque, et [docs/EXPLICATION.md](./docs/EXPLICATION.md) pour comprendre précisément comment le moteur fonctionne en interne.

## Structure du projet

Workspace Cargo multi-crates :

| Crate | Rôle | État |
|---|---|---|
| `pdf-core` | Moteur : lexer, objets COS, xref, arbre des pages, interpréteur de contenu, polices (simples et composites), filtres | Fonctionnel sur un sous-ensemble réel (voir STATUS.md) |
| `pdf-render` | Rasterisation CPU (`tiny-skia`) : chemins vectoriels, glyphes (TrueType/CFF intégrés et substitués système), images (JPEG + échantillons bruts, canal alpha `/SMask`), clip, rotation | Fonctionnel, comparé pixel par pixel à un corpus de référence |
| `pdf-render-gpu` | Rasterisation GPU (`wgpu` + `lyon`) : parité fonctionnelle avec `pdf-render`, branché dans `pdf-ui` avec repli automatique sur le CPU | Fonctionnel |
| `pdf-text` | Extraction de texte avec position par caractère, recherche, sélection | Fonctionnel |
| `pdf-app` | État de session (ouverture, navigation, rendu, recherche, cache) partagé entre `pdf-ui` et les futurs fronts | Fonctionnel |
| `pdf-cli` | Outil ligne de commande (`dump`, `render-info`, `render`, `text`) | Fonctionnel |
| `pdf-ui` | Viewer (`egui`/`eframe`) avec chrome natif macOS : menus système, ouverture/export natifs, glisser-déposer, plein écran, mode sombre ; navigation, zoom, recherche, miniatures, signets, défilement continu, sélection de texte | Fonctionnel, packagé en `.app`/`.dmg` (voir STATUS.md) |
| `pdf-edit` | Annotations (`/Highlight`), remplissage de champs AcroForm, journal `EditOp` + undo/redo, sauvegarde incrémentale | Fonctionnel au niveau moteur (pas encore d'interface `pdf-ui`) |

## Essayer

```bash
cargo build
cargo test --workspace

# Inspecter la structure d'un PDF
cargo run --bin pdf-cli -- dump chemin/vers/fichier.pdf

# Voir ce que l'interpréteur de contenu a produit pour une page
cargo run --bin pdf-cli -- render-info chemin/vers/fichier.pdf 0

# Rasteriser une page en PNG
cargo run --bin pdf-cli -- render chemin/vers/fichier.pdf sortie.png 0

# Extraire le texte d'une page
cargo run --bin pdf-cli -- text chemin/vers/fichier.pdf 0

# Ajouter une annotation de surlignage et sauvegarder incrémentalement
cargo run --bin pdf-cli -- highlight in.pdf out.pdf 0 100 600 300 630 1 1 0

# Remplir un champ de formulaire AcroForm
cargo run --bin pdf-cli -- fill-form in.pdf out.pdf nom_du_champ "valeur"

# Ouvrir le prototype de viewer graphique
cargo run --bin pdf-ui -- chemin/vers/fichier.pdf
```

Fixtures de test disponibles dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md)) : 25 PDF réels et structurellement variés (rotation, chiffrement RC4/AES-256, CJK avec polices composites, formulaires, corruptions diverses, JPEG RGB/CMYK, PDF/A-like...).

## Tests de rendu (comparaison pixel)

En plus des tests unitaires classiques, deux suites comparent le rendu à une image de référence sous seuil de tolérance :

```bash
# Compare le rendu CPU (pdf-render) à une image de référence par fixture
cargo test -p pdf-render --test golden

# Compare le rendu CPU et le rendu GPU entre eux, sur les mêmes fixtures
cargo test -p pdf-render-gpu --test cross_backend
```

Pour régénérer volontairement les images de référence après un changement de rendu voulu : `UPDATE_GOLDEN=1 cargo test -p pdf-render --test golden`.

## Documentation

- [architecture.md](./architecture.md) — document d'architecture cible complet : principes, découpage en couches du moteur PDF, choix techniques, modèle de données, risques.
- [sprint.md](./sprint.md) — plan de sprints dérivé de la roadmap par phases, coché sprint par sprint avec le statut réel de chaque item.
- [STATUS.md](./STATUS.md) — état précis du projet à date : ce qui marche, ce qui est simulé/placeholder, ce qui manque, avec pointeurs vers le code.
- [docs/EXPLICATION.md](./docs/EXPLICATION.md) — explication détaillée du fonctionnement interne du moteur, couche par couche.

## Choix techniques clés

- **Rust natif**, workspace Cargo.
- Codecs génériques implémentés : `flate2` (Flate), `zune-jpeg` (DCTDecode/JPEG, RGB et CMYK), plus un décodeur LZW et des prédicteurs PNG/TIFF écrits maison. Contours de glyphes via `ttf-parser` (TrueType et CFF/Type1C, polices simples et composites `/Type0`/CID). Rendu CPU via `tiny-skia`, rendu GPU via `wgpu`+`lyon` en parité fonctionnelle.
- Codecs pas encore implémentés : CCITT, JBIG2, JPX. Police pas encore supportée : Type1 historique (`/FontFile`, pré-CFF).
- UI : prototype `egui`/`eframe` avec chrome natif macOS (`objc2`/`objc2-app-kit` : `NSMenu`, `NSApplication.appearance`).
- Packaging : `cargo-bundle` produit un `.app` valide, empaqueté en `.dmg` via `hdiutil` — signature/notarisation Apple réelles pas encore faites (nécessitent un compte Apple Developer, non disponible dans cet environnement).

## Statut

Phases 0 à 3 (fondations, parsing, rendu CPU/GPU, UX viewer, chrome natif & packaging) fonctionnellement complètes : rendu vectoriel, texte (intégré + substitué système + composites CJK) et images (JPEG RGB/CMYK, `/SMask`) tous validés visuellement **et** par comparaison pixel automatisée ; back-end GPU en parité fonctionnelle avec le CPU ; viewer `pdf-ui` avec navigation, recherche, miniatures, signets, défilement continu, sélection de texte **et** chrome natif macOS (menus système, ouverture/export natifs, glisser-déposer, plein écran, mode sombre), packagé en `.app`/`.dmg`.

Phase 4 (annotations & formulaires) a un premier socle moteur fonctionnel : `pdf-edit` sait ajouter une annotation `/Highlight`, remplir un champ de formulaire texte (avec régénération de l'apparence visible au rendu), et propose un vrai historique undo/redo — le tout persisté par sauvegarde incrémentale (`pdf-core::writer` + `Document::save_incremental`) et vérifié bout en bout (sauvegarde, réouverture, rendu réel). Il manque encore l'interface `pdf-ui` pour déclencher ces opérations sans passer par `pdf-cli`/l'API Rust directement — voir [sprint.md](./sprint.md) Sprint 13-14.

Voir [sprint.md](./sprint.md) pour le détail sprint par sprint et [STATUS.md](./STATUS.md) pour une vue d'ensemble synthétique et à jour.
