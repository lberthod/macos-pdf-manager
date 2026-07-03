# macOS PDF Manager

Éditeur / visionneuse PDF **natif macOS**, écrit en **Rust**, distribué en `.dmg`, avec un **moteur PDF maison** (parsing et rendu écrits from scratch, sans dépendre de pdfium/MuPDF).

## Fonctionnalités visées

1. **Visualiser** des PDF (rendu fidèle, zoom, navigation, recherche texte).
2. **Annoter** (surlignage, notes, formes, signatures, remplissage de formulaires).
3. **Manipuler les pages** (réorganiser, supprimer, pivoter, fusionner, découper, insérer).
4. **Éditer le contenu**, y compris le texte existant et les objets vectoriels (périmètre progressif, voir plus bas).

> ⚠️ L'édition complète du texte d'un PDF avec un moteur écrit de zéro est un projet difficile : un PDF est une description de rendu (glyphes positionnés), pas un format éditable. Le périmètre réaliste et l'approche par phases sont détaillés dans [architecture.md](./architecture.md).

## État actuel (voir [STATUS.md](./STATUS.md) pour le détail précis)

Le projet a un **moteur PDF fonctionnel de bout en bout** sur un sous-ensemble réel de PDF : ouverture d'un fichier → xref (classique et streams PDF 1.5+) → arbre des pages → interprétation du flux de contenu → rendu CPU en PNG, avec de vraies métriques de police et de vrais contours de glyphes pour les polices TrueType intégrées.

Ce qui **ne fonctionne pas encore** : l'édition, l'annotation, la manipulation de pages, l'UI graphique, et le rendu de texte pour les polices non intégrées (le cas le plus courant en pratique). Voir [STATUS.md](./STATUS.md) pour la liste précise, fichier par fichier, de ce qui est fait et de ce qui manque.

## Structure du projet

Workspace Cargo multi-crates :

| Crate | Rôle | État |
|---|---|---|
| `pdf-core` | Moteur : lexer, objets COS, xref, arbre des pages, interpréteur de contenu, polices, filtres | Fonctionnel sur un sous-ensemble réel (voir STATUS.md) |
| `pdf-render` | Rasterisation CPU (`tiny-skia`) : chemins vectoriels + glyphes TrueType intégrés | Fonctionnel, partiel (pas d'images, pas de police système) |
| `pdf-cli` | Outil ligne de commande (`dump`, `render-info`, `render`) | Fonctionnel |
| `pdf-text` | Extraction / analyse / réécriture de la couche texte | Stub vide |
| `pdf-edit` | Opérations d'édition, journal, undo/redo | Stub vide |
| `pdf-app` | Logique applicative, état, contrôleur | Stub vide |
| `pdf-ui` | Interface graphique macOS | Stub vide |

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
```

Fixtures de test disponibles dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md)).

## Documentation

- [architecture.md](./architecture.md) — document d'architecture cible complet : principes, découpage en couches du moteur PDF, choix techniques, modèle de données, risques.
- [sprint.md](./sprint.md) — plan de sprints dérivé de la roadmap par phases, coché sprint par sprint avec le statut réel de chaque item.
- [STATUS.md](./STATUS.md) — état précis du projet à date : ce qui marche, ce qui est simulé/placeholder, ce qui manque, avec pointeurs vers le code.

## Choix techniques clés

- **Rust natif**, workspace Cargo.
- Codecs génériques implémentés : `flate2` (Flate), plus un décodeur LZW et des prédicteurs PNG/TIFF écrits maison (pas de crate dédiée nécessaire). Contours de glyphes via `ttf-parser` (TrueType uniquement pour l'instant). Rendu CPU via `tiny-skia`. Le moteur « maison » se concentre sur la logique PDF (structure, contenu, rendu sémantique).
- Codecs pas encore implémentés : JPEG (`DCTDecode`), CCITT, JBIG2, JPX.
- UI : pas encore commencée (prototype `egui` prévu, puis chrome natif macOS via `objc2`/`cacao`).
- Packaging : `cargo-bundle` + signature/notarisation Apple pour le `.dmg` — pas encore fait.

## Statut

Phases 0-1 (fondations + parsing du cœur) fonctionnellement complètes sur un corpus de test modeste (5 fixtures réels). Phase 2 (rendu) partiellement avancée : rendu vectoriel et rendu de texte (polices TrueType intégrées) tous deux validés visuellement de bout en bout. Voir [sprint.md](./sprint.md) pour le détail sprint par sprint et [STATUS.md](./STATUS.md) pour une vue d'ensemble synthétique et à jour.
