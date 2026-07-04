# macOS PDF Manager

Éditeur / visionneuse PDF **natif macOS**, écrit en **Rust**, distribué en `.dmg`, avec un **moteur PDF maison** (parsing et rendu écrits from scratch, sans dépendre de pdfium/MuPDF).

## Fonctionnalités visées

1. **Visualiser** des PDF (rendu fidèle, zoom, navigation, recherche texte).
2. **Annoter** (surlignage, notes, formes, signatures, remplissage de formulaires).
3. **Manipuler les pages** (réorganiser, supprimer, pivoter, fusionner, découper, insérer).
4. **Éditer le contenu**, y compris le texte existant et les objets vectoriels (périmètre progressif, voir plus bas).

> ⚠️ L'édition complète du texte d'un PDF avec un moteur écrit de zéro est un projet difficile : un PDF est une description de rendu (glyphes positionnés), pas un format éditable. Le périmètre réaliste et l'approche par phases sont détaillés dans [architecture.md](./architecture.md).

## État actuel (voir [STATUS.md](./STATUS.md) pour le détail précis)

Le projet a un **moteur PDF fonctionnel de bout en bout** sur un sous-ensemble réel de PDF : ouverture d'un fichier → xref (classique et streams PDF 1.5+) → arbre des pages → interprétation du flux de contenu → rendu CPU en PNG, avec de vraies métriques de police et de vrais contours de glyphes — polices TrueType et CFF/Type1C **intégrées**, comme polices standard **non intégrées** (substituées par les polices système macOS : Helvetica, Times, Courier...).

Ce qui **ne fonctionne pas encore** : l'édition, l'annotation, la manipulation de pages, l'UI graphique, les polices composites CJK, les images CCITT/JBIG2/JPX (le JPEG fonctionne). Voir [STATUS.md](./STATUS.md) pour la liste précise, fichier par fichier, de ce qui est fait et de ce qui manque, et [docs/EXPLICATION.md](./docs/EXPLICATION.md) pour comprendre précisément comment le moteur fonctionne en interne.

## Structure du projet

Workspace Cargo multi-crates :

| Crate | Rôle | État |
|---|---|---|
| `pdf-core` | Moteur : lexer, objets COS, xref, arbre des pages, interpréteur de contenu, polices, filtres | Fonctionnel sur un sous-ensemble réel (voir STATUS.md) |
| `pdf-render` | Rasterisation CPU (`tiny-skia`) : chemins vectoriels, glyphes (intégrés et substitués système), images (JPEG + échantillons bruts) | Fonctionnel, partiel (pas de clip, pas d'alpha) |
| `pdf-cli` | Outil ligne de commande (`dump`, `render-info`, `render`) | Fonctionnel |
| `pdf-ui` | Prototype de viewer (`egui`/`eframe`) : ouverture, navigation, zoom | Fonctionnel, minimal (pas de chrome natif macOS, voir STATUS.md) |
| `pdf-text` | Extraction / analyse / réécriture de la couche texte | Stub vide |
| `pdf-edit` | Opérations d'édition, journal, undo/redo | Stub vide |
| `pdf-app` | Logique applicative, état, contrôleur | Stub vide |

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

# Ouvrir le prototype de viewer graphique
cargo run --bin pdf-ui -- chemin/vers/fichier.pdf
```

Fixtures de test disponibles dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md)).

## Documentation

- [architecture.md](./architecture.md) — document d'architecture cible complet : principes, découpage en couches du moteur PDF, choix techniques, modèle de données, risques.
- [sprint.md](./sprint.md) — plan de sprints dérivé de la roadmap par phases, coché sprint par sprint avec le statut réel de chaque item.
- [STATUS.md](./STATUS.md) — état précis du projet à date : ce qui marche, ce qui est simulé/placeholder, ce qui manque, avec pointeurs vers le code.

## Choix techniques clés

- **Rust natif**, workspace Cargo.
- Codecs génériques implémentés : `flate2` (Flate), `zune-jpeg` (DCTDecode/JPEG), plus un décodeur LZW et des prédicteurs PNG/TIFF écrits maison. Contours de glyphes via `ttf-parser` (TrueType uniquement pour l'instant). Rendu CPU via `tiny-skia` (chemins, glyphes, images). Le moteur « maison » se concentre sur la logique PDF (structure, contenu, rendu sémantique).
- Codecs pas encore implémentés : CCITT, JBIG2, JPX.
- UI : pas encore commencée (prototype `egui` prévu, puis chrome natif macOS via `objc2`/`cacao`).
- Packaging : `cargo-bundle` + signature/notarisation Apple pour le `.dmg` — pas encore fait.

## Statut

Phases 0-1 (fondations + parsing du cœur) fonctionnellement complètes sur un corpus de test modeste (6 fixtures réels). Phase 2 (rendu) bien avancée : rendu vectoriel, texte (intégré + substitué système) et images JPEG tous validés visuellement de bout en bout, avec un premier prototype de viewer graphique (`pdf-ui`) fonctionnel en avance de phase. Voir [sprint.md](./sprint.md) pour le détail sprint par sprint et [STATUS.md](./STATUS.md) pour une vue d'ensemble synthétique et à jour.
