# macOS PDF Manager

Éditeur / visionneuse PDF **natif macOS**, écrit en **Rust**, distribué en `.dmg`, avec un **moteur PDF maison** (parsing et rendu écrits from scratch, sans dépendre de pdfium/MuPDF).

## Fonctionnalités visées

1. **Visualiser** des PDF (rendu fidèle, zoom, navigation, recherche texte).
2. **Annoter** (surlignage, notes, formes, signatures, remplissage de formulaires).
3. **Manipuler les pages** (réorganiser, supprimer, pivoter, fusionner, découper, insérer).
4. **Éditer le contenu**, y compris le texte existant et les objets vectoriels (périmètre progressif, voir plus bas).

> ⚠️ L'édition complète du texte d'un PDF avec un moteur écrit de zéro est un projet difficile : un PDF est une description de rendu (glyphes positionnés), pas un format éditable. Le périmètre réaliste et l'approche par phases sont détaillés dans [architecture.md](./architecture.md).

## Structure du projet

Workspace Cargo multi-crates (à créer au Sprint 0) :

| Crate | Rôle |
|---|---|
| `pdf-core` | Moteur : parsing, modèle, contenu, rendu, écriture |
| `pdf-text` | Extraction / analyse / réécriture de la couche texte |
| `pdf-render` | Rasterisation & rendu vectoriel (CPU + GPU) |
| `pdf-edit` | Opérations d'édition, journal, undo/redo |
| `pdf-app` | Logique applicative, état, contrôleur |
| `pdf-ui` | Interface graphique macOS |
| `pdf-cli` | Outil ligne de commande (debug, tests, batch) |

## Documentation

- [architecture.md](./architecture.md) — document d'architecture complet : principes, découpage en couches du moteur PDF, choix techniques, modèle de données, risques.
- [sprint.md](./sprint.md) — plan de sprints dérivé de la roadmap par phases.

## Choix techniques clés

- **Rust natif**, workspace Cargo.
- Codecs génériques (deflate, JPEG) et rasterisation de glyphes via des crates éprouvées (`flate2`, `zune-jpeg`, `ttf-parser`, `swash`, `tiny-skia`, `wgpu`/`lyon`) ; le moteur « maison » se concentre sur la logique PDF (structure, contenu, rendu sémantique, écriture).
- UI : prototype `egui`, chrome natif macOS via `objc2`/`cacao`.
- Packaging : `cargo-bundle` + signature/notarisation Apple pour le `.dmg`.

## Statut

Projet en phase de cadrage. Voir [sprint.md](./sprint.md) pour l'état d'avancement et les points à trancher avant le démarrage ([architecture.md §12](./architecture.md#12-points-à-trancher-avec-le-développeur-avant-le-démarrage)).
