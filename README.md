<p align="center">
  <img src="pdf-ui/icons/icon_512.png" width="128" height="128" alt="Icône PapyrusPDF">
</p>

<h1 align="center">PapyrusPDF</h1>

<p align="center">
  Éditeur / visionneuse PDF <strong>natif macOS</strong>, écrit en <strong>Rust</strong>,<br>
  avec un <strong>moteur PDF maison</strong> (parsing et rendu écrits from scratch, sans pdfium/MuPDF).
</p>

<p align="center">
  <a href="https://github.com/lberthod/macos-pdf-manager/releases/latest">⬇️ Télécharger la dernière version (.dmg)</a>
</p>

---

## Installation

1. Télécharger le `.dmg` le plus récent depuis [Releases](https://github.com/lberthod/macos-pdf-manager/releases).
2. L'ouvrir, glisser **PapyrusPDF.app** dans le dossier `Applications`.
3. Lancer l'app depuis `Applications` ou Spotlight.

Le `.dmg` et l'app qu'il contient sont **signés (Developer ID Application) et notarisés par Apple** : macOS les ouvre normalement, sans l'avertissement "développeur non identifié" habituel pour une app non distribuée via l'App Store. Voir [Distribution & notarisation](#distribution--notarisation) pour le détail du processus, si tu veux produire ton propre `.dmg` signé.

## Fonctionnalités

- **Visualiser** : rendu fidèle (CPU et GPU), zoom (boutons, molette, pincement trackpad centré sur le curseur, ajuster à la largeur/à la page), navigation (page à page, aller à une page, recherche plein texte insensible à la casse/aux accents avec surlignage), miniatures, signets, défilement continu, sélection de texte (glisser, double/triple-clic) et copie, export du texte en `.txt`, mode sombre, plein écran.
- **Annoter** : surligner/souligner/barrer une sélection, ajouter du texte libre, remplacer un texte existant (superposition), annuler/rétablir illimité, enregistrer en place ou exporter une copie/version optimisée.
- **Manipuler les pages** : réordonner par glisser-déposer, supprimer, pivoter, insérer (page vierge, image JPEG, autre PDF), fusionner un document, extraire une sélection de pages vers un nouveau fichier.
- **Imprimer** : délégation à Aperçu (dialogue système, aperçu, sélection de pages).
- **Onglets multi-documents** : plusieurs PDF ouverts dans une seule fenêtre.
- **Ouvrir des PDF chiffrés** (mot de passe utilisateur vide) : RC4, AES-128 et AES-256 gérés.

> ⚠️ L'édition chirurgicale du texte existant (modifier un glyphe déjà présent dans le flux de contenu, plutôt que le masquer et le redessiner par superposition) reste hors périmètre : un PDF est une description de rendu (glyphes positionnés), pas un format éditable — voir [architecture.md](./architecture.md#73-édition-du-texte-existant--le-vrai-défi) pour le détail de cette limite et l'approche par phases retenue.

Ce qui ne fonctionne pas encore : Quick Look, formes/notes/signatures, remplissage de formulaire au clic (le moteur le sait faire, pas encore câblé dans l'interface), Type1 historique (police pré-CFF), images CCITT/JBIG2/JPX, PDF chiffré avec un vrai mot de passe (pas de dialogue de saisie), accessibilité VoiceOver. Voir [STATUS.md](./STATUS.md) et [audit50quest.md](./audit50quest.md) pour le détail précis, fonctionnalité par fonctionnalité.

## Structure du projet

Workspace Cargo multi-crates :

| Crate | Rôle |
|---|---|
| `pdf-core` | Moteur : lexer, objets COS, xref, arbre des pages, interpréteur de contenu, polices (simples et composites), filtres, déchiffrement `/Encrypt` |
| `pdf-render` | Rasterisation CPU (`tiny-skia`) |
| `pdf-render-gpu` | Rasterisation GPU (`wgpu` + `lyon`), parité fonctionnelle avec `pdf-render` |
| `pdf-text` | Extraction de texte avec position par caractère, recherche, sélection |
| `pdf-edit` | Annotations, remplissage de formulaires, édition de texte (superposition), manipulation de pages, undo/redo, sauvegarde incrémentale |
| `pdf-app` | État de session partagé entre `pdf-ui` et les futurs fronts |
| `pdf-ui` | Application graphique (`egui`/`eframe`) avec chrome natif macOS — c'est elle qui est packagée en `.app`/`.dmg` |
| `pdf-cli` | Outil ligne de commande pour inspecter/manipuler un PDF sans interface graphique |

## Distribution & notarisation

Le `.dmg` publié dans les [Releases](https://github.com/lberthod/macos-pdf-manager/releases) est produit ainsi :

```bash
# 1. Compiler et empaqueter en .app (cargo-bundle lit [package.metadata.bundle] dans pdf-ui/Cargo.toml)
cargo bundle --release -p pdf-ui --format osx

# 2. Signer avec une identité Developer ID Application + hardened runtime
#    (requis par la notarisation)
codesign --force --deep --options runtime --timestamp \
  --sign "Developer ID Application: <Nom> (<Team ID>)" \
  "target/release/bundle/osx/PapyrusPDF.app"

# 3. Soumettre à Apple pour notarisation et attendre le verdict
ditto -c -k --keepParent "target/release/bundle/osx/PapyrusPDF.app" PapyrusPDF.zip
xcrun notarytool submit PapyrusPDF.zip --keychain-profile "papyruspdf-notary" --wait

# 4. Agrafer le ticket de notarisation à l'app
xcrun stapler staple "target/release/bundle/osx/PapyrusPDF.app"

# 5. Construire le .dmg (app + lien symbolique vers /Applications)
mkdir -p /tmp/dmg_staging
cp -R "target/release/bundle/osx/PapyrusPDF.app" /tmp/dmg_staging/
ln -s /Applications /tmp/dmg_staging/Applications
hdiutil create -volname "PapyrusPDF" -srcfolder /tmp/dmg_staging -ov -format UDZO PapyrusPDF.dmg

# 6. Signer le .dmg lui-même (sinon Gatekeeper le rejette une fois téléchargé,
#    même si l'app qu'il contient est signée), puis le notariser et l'agrafer à son tour
codesign --force --timestamp --sign "Developer ID Application: <Nom> (<Team ID>)" PapyrusPDF.dmg
xcrun notarytool submit PapyrusPDF.dmg --keychain-profile "papyruspdf-notary" --wait
xcrun stapler staple PapyrusPDF.dmg
```

Point important découvert en le faisant : la notarisation du `.dmg` ne suffit pas si le `.dmg` lui-même n'est pas signé — Gatekeeper vérifie la signature du fichier réellement téléchargé (le conteneur `.dmg`), pas seulement celle de l'app qu'il contient. D'où l'étape 6, en plus de l'étape 2/3/4 sur l'`.app`.

Les identifiants de notarisation (Apple ID, Team ID, mot de passe d'application) ne sont jamais committés : `xcrun notarytool store-credentials` les enregistre une fois dans le Trousseau macOS, référencés ensuite par un simple nom de profil (`--keychain-profile`).

Vérification après coup :

```bash
spctl -a -vv --type execute PapyrusPDF.app   # doit répondre "accepted", source=Notarized Developer ID
hdiutil verify PapyrusPDF.dmg                 # intégrité du disque image
```

## Documentation

- [architecture.md](./architecture.md) — document d'architecture cible : principes, découpage en couches du moteur PDF, choix techniques, modèle de données, risques.
- [sprint.md](./sprint.md) — plan de sprints, coché sprint par sprint avec le statut réel de chaque item.
- [STATUS.md](./STATUS.md) — état précis du projet à date : ce qui marche, ce qui est simulé/placeholder, ce qui manque, avec pointeurs vers le code.
- [audit50quest.md](./audit50quest.md) — audit ligne par ligne contre une grille de 50 fonctionnalités attendues d'un viewer/éditeur PDF, avec score de couverture.
- [analyse_sprint.md](./analyse_sprint.md) — plan d'action dérivé de cet audit.
- [docs/EXPLICATION.md](./docs/EXPLICATION.md) — explication détaillée du fonctionnement interne du moteur, couche par couche.

## Développement

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check

# Lancer le viewer directement (sans passer par le .dmg)
cargo run -p pdf-ui -- chemin/vers/fichier.pdf

# Outil en ligne de commande (dump, render, text, fill-form, merge, split, highlight...)
cargo run --bin pdf-cli -- dump chemin/vers/fichier.pdf
```

Fixtures de test dans [pdf-core/tests/fixtures/](./pdf-core/tests/fixtures/) (voir leur [README](./pdf-core/tests/fixtures/README.md)) : 25 PDF réels et structurellement variés (rotation, chiffrement RC4/AES-256, CJK avec polices composites, formulaires, corruptions diverses, JPEG RGB/CMYK, PDF/A-like...).

Deux suites comparent le rendu à une image de référence sous seuil de tolérance :

```bash
cargo test -p pdf-render --test golden          # CPU vs image de référence
cargo test -p pdf-render-gpu --test cross_backend  # CPU vs GPU
```

## Choix techniques clés

- **Rust natif**, workspace Cargo.
- Codecs génériques implémentés : `flate2` (Flate), `zune-jpeg` (DCTDecode/JPEG, RGB et CMYK), plus un décodeur LZW et des prédicteurs PNG/TIFF écrits maison. Contours de glyphes via `ttf-parser` (TrueType et CFF/Type1C, polices simples et composites `/Type0`/CID). Rendu CPU via `tiny-skia`, rendu GPU via `wgpu`+`lyon` en parité fonctionnelle.
- Déchiffrement `/Encrypt` via des primitives cryptographiques auditées (`md-5`, `sha2`, `aes`, `cbc`) plutôt que réimplémentées à la main, sauf RC4 (algorithme trivial, chiffrement obsolète, validé contre le vecteur de test RFC officiel).
- Codecs pas encore implémentés : CCITT, JBIG2, JPX. Police pas encore supportée : Type1 historique (`/FontFile`, pré-CFF).
- UI : `egui`/`eframe` avec chrome natif macOS (`objc2`/`objc2-app-kit` : `NSMenu`, `NSApplication.appearance`), thread de rendu en arrière-plan dédié pour les miniatures/le défilement continu.
- Packaging : `cargo-bundle` + `hdiutil`, signé (Developer ID Application, hardened runtime) et notarisé par Apple — voir [Distribution & notarisation](#distribution--notarisation).
