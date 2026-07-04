# UIX Analyse — Dossier de conception & plan d'exécution (mis à jour à l'état réel du projet)

Éditeur / Visionneuse PDF natif macOS — Rust, moteur maison

**Version :** 3.0 (révision à partir de l'état réel du code après Sprints 11-16)
**Date :** 2026-07-04
**Destinataire :** développeur(euse) Rust en charge de la réalisation
**Statut :** projet en cours — Phases 0 à 5 (viewer, chrome natif, socle d'édition, manipulation de pages) largement avancées ; reste à faire : interface `pdf-ui` pour déclencher l'édition, édition de texte (Sprint 17+, entamée), durcissement/distribution (Sprint 18+)

> Ce document reprend et met à jour la version 1.0 du dossier de conception initial, en la confrontant à l'état réel du dépôt (voir [STATUS.md](./STATUS.md) pour le détail vérifiable commande par commande, et [sprint.md](./sprint.md) pour le suivi sprint par sprint). Les sections ci-dessous ne dupliquent pas ces deux fichiers : elles renvoient vers eux et signalent explicitement les écarts entre le plan initial et la réalité.

---

## 1. Résumé exécutif

Construire une application native macOS (`.dmg`) en Rust, avec un moteur PDF écrit de zéro pour la logique PDF (parsing, modèle document, interprétation de contenu, résolution de polices), permettant de visualiser, annoter, manipuler les pages et éditer le contenu de PDF.

Décisions actées et **confirmées par le code existant** :

| Sujet | Décision v1.0 | État réel |
|---|---|---|
| Plateforme | macOS natif, `.dmg` signé/notarisé | **Chrome natif fait** (Sprint 11-12) : vraie `NSMenu` (`objc2`/`objc2-app-kit`), mode sombre système, plein écran, glisser-déposer ; packaging `.app`/`.dmg` fonctionnel via `cargo-bundle`+`hdiutil` (vérifié `hdiutil verify`). **Reste :** signature Developer ID et notarisation réelles (identifiants Apple Developer non disponibles dans cet environnement, seule une signature ad-hoc existe) |
| Moteur | Maison (logique PDF from scratch) | **Confirmé et livré** : lexer, xref, filtres, modèle document, interpréteur de contenu, résolution de polices sont bien maison (`pdf-core`), sur ~40 opérateurs de contenu, polices simples et composites `/Type0`/CID |
| Frontière maison / crates | À confirmer en Sprint 0 | **Tranchée par la pratique** : codecs génériques (`flate2` implicite via filtres maison en fait écrits à la main, `zune-jpeg` pour JPEG), rasterisation (`tiny-skia` CPU, `wgpu`+`lyon` GPU), police bas niveau (`ttf-parser`), GUI (`egui`/`eframe`, `rfd` pour les dialogues), chrome natif (`objc2`) — la logique PDF elle-même reste 100 % maison |
| Édition texte | Cible « complète », exécution progressive (superposition d'abord) | **Socle moteur livré** (Sprints 13-16) : annotations `/Highlight`, remplissage de formulaire AcroForm, undo/redo (`EditOp`), sauvegarde incrémentale, manipulation de pages (insertion/suppression/déplacement/rotation), fusion/découpage de documents, export optimisé (GC). **Édition de texte proprement dite (Sprint 17+) entamée** : 6a (ajout de texte, `/FreeText`) et 6b (remplacement par superposition) implémentés et testés dans `pdf-edit` — voir §6.3, changement **non encore commité** au moment de la rédaction. Aucune de ces briques n'a d'interface `pdf-ui` : seuls `pdf-cli`/l'API Rust directe les exercent |
| Livrable | Documents Markdown versionnés avec le code | Confirmé : `architecture.md`, `STATUS.md`, `sprint.md`, et ce document |

**Réalité à garder en tête (inchangée depuis v1.0) :** le moteur maison n'apporte pas d'avantage de vitesse sur PDFium/MuPDF. La valeur du produit se joue sur l'UX native et la maîtrise complète du code. **Constat après 16 sprints de développement réel :** cette hypothèse se vérifie — le moteur de rendu (CPU et GPU) est solide et testé (189 tests, harnais de comparaison pixel sur 23/12 fixtures), le chrome natif macOS est fait (Sprint 11-12), et un socle d'édition complet existe côté moteur (annotations, formulaires, undo/redo, manipulation de pages, fusion/découpage — Sprints 13-16). **L'écart le plus visible avec un « vrai » éditeur reste l'absence totale d'interface `pdf-ui` pour déclencher ces opérations** : tout existe en Rust/`pdf-cli`, rien n'est cliquable. C'est la prochaine étape qui matérialise le pari du produit.

---

## 2. Architecture — état réel du workspace

Le workspace Cargo comporte aujourd'hui 8 crates (tous membres, tous compilent, aucun n'est un placeholder sauf `pdf-edit`) :

| Crate | Rôle prévu (v1.0) | État réel |
|---|---|---|
| `pdf-core` | Lexer, objets/xref, filtres, modèle document, interpréteur de contenu, polices | **Fait**, y compris récupération d'erreur, object streams, polices composites `/Type0`/CID (`CIDFontType0` et `CIDFontType2`), `/ToUnicode`, `/Outlines` |
| `pdf-render` | Rasterisation CPU (référence) | **Fait** — `tiny-skia`, clip réel (`W`/`W*`), rotation, images avec alpha, harnais golden (`tests/golden.rs`, 23 fixtures) |
| `pdf-render-gpu` | Rasterisation GPU (wgpu/Metal) | **Fait, en parité fonctionnelle avec le CPU** — `wgpu` + `lyon`, stencil buffer pour le clip, cache de glyphes, branché dans `pdf-ui` avec repli automatique sur CPU si pas d'adaptateur. Comparaison pixel CPU/GPU automatisée (`tests/cross_backend.rs`, 12 fixtures) |
| `pdf-text` | Extraction, sélection, recherche | **Fait** pour l'extraction linéaire, la recherche avec surlignage, la sélection (hit-test caractère). Pas de reconstruction par blocs/colonnes |
| `pdf-edit` | Opérations d'édition, journal, undo/redo, sauvegarde incrémentale | **Fait, socle moteur complet** (Sprints 13-16, + travail Sprint 17+ en cours non commité) : annotations `/Highlight` et `/FreeText` (ajout de texte + remplacement par superposition), remplissage de champ AcroForm texte, undo/redo (`EditOp`), manipulation de pages (insertion/suppression/déplacement/rotation), fusion/découpage de documents, export optimisé (GC par reconstruction). **Aucune interface `pdf-ui`** ne déclenche encore ces opérations |
| `pdf-app` | État applicatif, contrôleur, orchestration | **Fait** — `Session` porte document, navigation, rendu (avec cache FIFO 32 entrées), recherche, sélection, table des matières (chacun avec son propre cache). **N'expose pas encore `pdf-edit`** (pas d'`EditSession` intégrée à `Session`, pas d'undo/redo déclenchable depuis `pdf-ui`), pas de multi-documents |
| `pdf-ui` | Interface macOS (cible : rendu wgpu + chrome natif AppKit via `objc2`) | **Fait pour le chrome natif, pas pour l'édition** (Sprint 11-12) — vraie `NSMenu` système (`objc2`/`objc2-app-kit`), raccourcis ⌘O/⌘S/⌘W/⌘Q/⌃⌘F/⌘M, mode sombre réel, glisser-déposer, backend GPU `wgpu` branché. **Aucun outil d'édition câblé** : pas de surlignage/annotation à la souris, pas de clic sur un champ de formulaire, pas de réorganisation de pages, ⌘Z/⌘⇧Z prévus dans le menu mais pas branchés |
| `pdf-cli` | Outil debug/tests/batch | **Fait** — `dump`, `render`, `render-info`, `text`, `highlight`, `fill-form`, `insert-blank-page`, `insert-image-page`, `delete-page`, `move-page`, `rotate-page`, `merge`, `split`, `optimize` |

**Décision de dépendance actée pour le chrome natif** (`sprint.md` Sprint 11-12) : `objc2` + crates officielles par framework (`objc2-foundation`, `objc2-app-kit`, `objc2-quartz-core`) plutôt que `cacao`, pour ne pas mélanger deux couches de bindings Objective-C. **Exécutée** : `pdf-ui/src/native_menu.rs` implémente le menu système, la classe `MenuTarget` (`objc2::define_class!`) et le pont vers `egui` par canal MPSC.

Invariants de conception (v1.0) — état de mise en œuvre :

- Document chargé immuable ; modifications = journal d'opérations → **fait** (`pdf-edit::EditOp` capture avant/après par objet modifié, `undo`/`redo` restaurent l'un ou l'autre ; limite connue : les objets **nouvellement créés** par une opération restent alloués après un `undo`, nettoyage réel via `export_optimized`).
- Rasterisation hors du thread UI → **toujours pas fait** ; le rendu reste synchrone dans `pdf-app::Session` (mis en cache, mais pas sur un pool de threads séparé), malgré le passage au chrome natif AppKit au Sprint 11-12. Risque non résorbé, voir §8.
- Résolution paresseuse des objets → **fait** (cache de résolution dans `pdf-core::document`).
- Parser tolérant aux fichiers malformés → **fait** (reconstruction xref par balayage d'octets, repli sur recherche `/Type /Catalog`).
- Zéro `unsafe` non justifié → à auditer explicitement (pas de revue dédiée à date dans ce document) ; le chrome natif introduit désormais de l'`unsafe`/FFI Objective-C via `objc2`, à couvrir par cet audit quand il sera fait.

---

## 3. UX cible — état réel face au MoSCoW de v1.0

| Priorité | Item | État réel |
|---|---|---|
| **Must** | Scroll/zoom fluide | Fait (`egui`, molette+Ctrl, pincement trackpad, scroll continu virtualisé) |
| **Must** | Miniatures | Fait (panneau latéral, cache) |
| **Must** | Menus & raccourcis natifs | **Fait** (Sprint 11-12) — vraie `NSMenu` : Fichier (⌘O/⌘S/⌘W), Affichage (mode sombre, ⌃⌘F plein écran), Fenêtre (⌘M), ⌘Q. **⌘Z/⌘⇧Z prévus dans le menu mais volontairement pas branchés** (pas d'undo/redo `pdf-edit` exposé par `pdf-app`/`pdf-ui`), ⌘F pas câblé non plus |
| **Must** | Mode sombre | **Fait** (Sprint 11-12) — bascule réelle `NSApplication.appearance`, synchronisée avec les couleurs `egui` |
| **Must** | Sélection + copier texte | Fait (glisser-souris, ⌘C, bouton), limité au mode page unique |
| **Must** | Recherche ⌘F | Fait au niveau fonctionnel (recherche + surlignage) ; **raccourci ⌘F toujours pas câblé** dans le menu natif |
| **Must** | Annotation + surlignage | **Moteur fait, UI pas faite** — `pdf-edit` gère `/Highlight` et `/FreeText` (ajout + remplacement par superposition, voir §6.3), testé bout en bout et via `pdf-cli`, mais **aucun outil de surlignage/annotation à la souris** dans `pdf-ui` |
| **Must** | Réorganisation de pages | **Moteur fait, UI pas faite** — `pdf-edit::EditSession` (insertion/suppression/déplacement/rotation, fusion/découpage) testé bout en bout et via `pdf-cli`, mais aucune interaction glisser-déposer/bouton dans `pdf-ui` |
| **Must** | Undo/redo | **Moteur fait, UI pas faite** — `pdf-edit::EditOp` testé (capture avant/après, `undo`/`redo`), mais pas intégré à `pdf-app::Session` ni déclenchable depuis `pdf-ui` |
| **Must** | Ouverture paresseuse | Fait (résolution paresseuse des objets) |
| **Must** | Navigation clavier | Partielle — pas de tab order/accessibilité de base formalisée |
| **Should** | Onglets natifs | Pas fait |
| **Should** | Quick Look | Pas fait (nécessiterait une extension d'app séparée, hors périmètre d'un binaire `cargo`) |
| **Should** | Édition texte par superposition | **Entamé** — 6a (ajout de texte `/FreeText`) et 6b (remplacement par superposition) implémentés et testés dans `pdf-edit`, non commités au moment de la rédaction ; 6c (édition chirurgicale du flux) reste hors périmètre. Pas d'UI |
| **Should** | Inspecteur | Pas fait |
| **Should** | Fusion/split | **Moteur fait** (`pdf-edit::merge_document`/`extract_pages`, `pdf-cli merge`/`split`), pas d'UI |
| **Should** | VoiceOver | Pas fait |
| **Could** | Zoom sémantique, Continuity, Spotlight, iCloud | Pas fait |

**Constat :** l'axe « viewer » est couvert de bout en bout, y compris l'intégration système macOS (menus, raccourcis, mode sombre, packaging — Sprint 11-12). Ce qui manque désormais structurellement, ce n'est plus l'intégration système mais **l'interface d'édition elle-même** : le moteur `pdf-edit` couvre annotations, formulaires, undo/redo, manipulation de pages et une bonne partie de l'ajout/remplacement de texte, mais rien de tout cela n'est cliquable dans `pdf-ui`. C'est le nouvel écart entre « bon viewer natif » et « éditeur utile au quotidien ».

---

## 4. Épics — état réel

| # | Épic | Valeur | Dépend de | État réel |
|---|---|---|---|---|
| E0 | Fondations & outillage | Base saine, CI, corpus de test | — | **Fait** (25 fixtures, CI fmt/clippy/tests, harnais golden pixel) |
| E1 | Parser & modèle document | Ouvrir/lire n'importe quel PDF | E0 | **Fait** (xref classique + streams, object streams, récupération d'erreur, filtres majeurs) |
| E2 | Rendu (viewer visuel) | Afficher fidèlement une page | E1 | **Fait** (CPU + GPU en parité, polices intégrées + substituées + composites, images avec alpha, clip, rotation) |
| E3 | Couche texte | Sélection, copier, recherche | E2 | **Fait** (extraction linéaire, sélection, recherche avec surlignage) |
| E4 | UX viewer natif macOS | Produit démontrable en lecture | E2, E3 | **Fait** (Sprint 11-12) — riche fonctionnellement (miniatures, signets, scroll continu, zoom, GPU) **et natif** (chrome AppKit via `objc2`, packaging `.app`/`.dmg` fonctionnel, signature ad-hoc seulement) |
| E5 | Annotations & formulaires | « Éditeur » utile | E4 | **Moteur fait (Sprint 13-14), UI pas faite** — `/Highlight`, remplissage AcroForm texte, undo/redo, sauvegarde incrémentale, tout testé bout en bout ; aucune interaction `pdf-ui` |
| E6 | Manipulation de pages | Réorganiser/fusionner/split | E1, E4 | **Moteur fait (Sprint 15-16), UI pas faite** — insertion/suppression/déplacement/rotation, fusion/découpage, export optimisé (GC), tout testé bout en bout ; aucune interaction `pdf-ui` |
| E7 | Édition de texte (progressive) | Modifier le contenu | E3, E5 | **Entamé** — 6a (ajout `/FreeText`) et 6b (remplacement par superposition) implémentés et testés côté moteur (non commité) ; 6c (édition chirurgicale) pas commencé ; aucune UI |
| E8 | Durcissement & distribution | Robustesse, `.dmg`, sécurité | tous | **Packaging `.app`/`.dmg` fait** (Sprint 11-12), signature/notarisation réelles restent à faire (compte développeur Apple non disponible dans cet environnement) ; chiffrement détecté mais non déchiffré ; pas de fuzzing formalisé au-delà du corpus de corruption manuelle |

**Chemin critique révisé :** E0 → E1 → E2 → E3 → E4 sont clos. **Le blocage actuel du chemin critique est l'interface `pdf-ui` pour E5/E6/E7** (le moteur d'édition existe et est testé, mais rien n'est déclenchable depuis l'app), pas le moteur de rendu ni le chrome natif — contrairement à la situation de départ où le risque principal perçu portait sur le moteur de rendu, puis (v2.0) sur le chrome natif.

---

## 5. Cadre agile & re-planification

Le cadre décrit en v1.0 (sprints de 2 semaines, story points Fibonacci, Definition of Ready/Done, cérémonies, phase gates par épic, règles de re-priorisation, spikes, budget de risque pour l'édition texte) **reste valide sans changement** et n'est pas reproduit ici in extenso — voir la version 1.0 conservée dans l'historique, ou considérer les règles suivantes comme actives :

- Phase gate à la fin de chaque épic — **appliquée dans les faits** : `sprint.md` marque explicitement chaque sprint « Statut réel » avec les réserves restantes plutôt que de le clore artificiellement (ex. Sprint 3-4 : critère « plusieurs centaines de PDF » non atteint littéralement, mais clos au sens fonctionnel ; Sprint 13-14/15-16 : « socle moteur » clos explicitement sans l'UI, voir §6.2).
- Trigger de re-découpe à 150 % d'estimation — pas d'historique de vélocité chiffré disponible dans ce dépôt ; à instrumenter si un outil de suivi externe est mis en place.
- Budget de risque sur l'édition texte (E7) — **entamé** : 6a/6b implémentés côté moteur (voir §6.3), 6c (édition chirurgicale) toujours traité comme un projet de recherche séparé.

**Tableau de bord actuel (constats extraits de STATUS.md) :**

- 189 tests automatisés, tous verts (`cargo test --workspace`) — 12 dans `pdf-edit` seul, dont les nouveaux tests non commités de 6a/6b.
- `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre.
- Corpus : 25 fixtures (diversité structurelle large, pas encore de volume — voir §4 de STATUS.md).
- Écart pixel : harnais en place (`pdf-render/tests/golden.rs`, 23 fixtures ; `pdf-render-gpu/tests/cross_backend.rs`, 12 fixtures).
- Bugs réels trouvés et corrigés en cours de route : `/Rotate` ignoré au rendu, message d'erreur trompeur sur PDF chiffré, décodage JPEG CMYK cassé, cache d'ordre de pages périmé après `undo` (Sprint 15-16) — signe que le corpus élargi et les tests bout en bout remplissent leur rôle.

---

## 6. Backlog — sprints réalisés vs sprints restants

Le découpage effectivement suivi (`sprint.md`) diffère de la numérotation sprint-par-sprint de v1.0 : les phases ont été regroupées en sprints de 2 semaines couvrant chacun une tranche de l'architecture (voir `architecture.md §9`). Ci-dessous, le même contenu que v1.0 mais reclassé « fait » / « restant », avec renvoi vers les preuves dans `sprint.md`.

### 6.1 Sprints clos (Phases 0-3, partie 1) — détail dans sprint.md

- **Sprint 0** (Fondations) — workspace, CI, corpus 25 fixtures, harnais golden pixel, `pdf-cli` minimal. **Clos.**
- **Sprint 1-2** (Lexer & objets COS) — lexer tolérant, modèle `Object`, parser d'objets indirects. **Clos.**
- **Sprint 3-4** (Xref & résolution document) — xref classique + streams, object streams, `/Prev`, récupération d'erreur, filtres (Flate/ASCIIHex/ASCII85/LZW + prédicteurs). **Clos** (au sens fonctionnel ; réserve sur le volume de corpus, voir §5).
- **Sprint 5-6** (Modèle document & interpréteur) — arbre de pages, ~40 opérateurs de contenu, clip suivi, `DisplayList`. **Clos.**
- **Sprint 7-8** (Polices & rendu CPU) — TrueType/CFF intégrés, substitution système, `/ToUnicode`, `/Type0`/CID (`CIDFontType0` + `CIDFontType2`), rasteriseur `tiny-skia`, images JPEG+`/SMask`, prototype `egui` (fait en avance de phase). **Clos** (Type1 historique `/FontFile` hors périmètre, non fait).
- **Sprint 9-10** (GPU & UX viewer) — back-end `wgpu`/`lyon` en parité avec le CPU, zoom + scroll continu, cache de rendu/texte, recherche avec surlignage, sélection de texte, miniatures, signets. **Clos.**

**Bilan des sprints clos :** couvrent la quasi-totalité des épics E0-E3 et une large partie fonctionnelle de E4. C'est un viewer PDF complet et testé.

### 6.2 Sprints clos (Phases 3-5, chrome natif & édition moteur)

- **Sprint 11-12** (Chrome natif & packaging, Épic E4) — vraie `NSMenu` système via `objc2`/`objc2-app-kit` (menus Fichier/Affichage/Fenêtre, raccourcis ⌘O/⌘S/⌘W/⌘M/⌃⌘F/⌘Q), classe Objective-C `MenuTarget` définie côté Rust (`objc2::define_class!`) pour les actions propres à l'app, glisser-déposer géré nativement par `egui`/`winit`, mode sombre réel (`NSApplication.appearance`), packaging `.app`/`.dmg` fonctionnel (`cargo-bundle` + `hdiutil`, vérifié `hdiutil verify`). **Clos pour le fonctionnel** ; réserve : signature Developer ID / notarisation réelles non faites (compte développeur Apple non disponible dans cet environnement, signature ad-hoc du linker seulement), Quick Look hors périmètre (extension d'app séparée), ⌘Z/⌘⇧Z/⌘F prévus mais pas branchés (attendent une intégration `pdf-edit`/recherche dans le menu).
- **Sprint 13-14** (Annotations & formulaires, Épic E5) — sérialisation d'objets (`pdf-core::writer`), sauvegarde incrémentale (`Document::save_incremental`, append + xref chaînée par `/Prev`), rendu des annotations via `/AP /N` (`Interpreter::run_page_with_annotations`), `pdf-edit::EditSession` : annotation `/Highlight` (`add_highlight_annotation`), remplissage de champ AcroForm texte (`set_form_field_value`), undo/redo (`EditOp`, capture avant/après par objet modifié). **Clos au sens moteur** (testé bout en bout : ajout, sauvegarde, réouverture, rendu réel, et via `pdf-cli highlight`/`fill-form`) ; **aucune interface `pdf-ui`**.
- **Sprint 15-16** (Manipulation de pages, Épic E6) — `ensure_flat_page_tree` (aplatissement paresseux de l'arbre `/Pages`), `insert_blank_page`/`insert_image_page`/`delete_page`/`move_page`/`rotate_page`, `copy_object_recursive` + `write_standalone` pour la fusion (`merge_document`/`insert_pages_from`) et le découpage (`extract_pages`), `export_optimized` (garbage collector par reconstruction). Bug réel trouvé et corrigé en écrivant les tests : cache d'ordre de pages périmé après `undo` (`refresh_page_tree_order`). **Clos au sens moteur** (testé bout en bout et via `pdf-cli insert-blank-page`/`merge`/`split`/`optimize`/etc.) ; **aucune interface `pdf-ui`**.

**Bilan de ces trois sprints :** le moteur d'édition (`pdf-edit`) est passé d'un stub vide à un ensemble complet et testé d'opérations (annotations, formulaires, undo/redo, pages, fusion/découpage, GC). Le viewer natif macOS (E4) est clos. **Le nouveau chemin critique est l'interface `pdf-ui`** pour rendre tout cela utilisable sans passer par `pdf-cli`/l'API Rust directe.

### 6.3 Sprint en cours — Sprint 17+ (édition de texte, Épic E7)

**Objectif :** livrer une édition de texte réaliste par étapes (voir `architecture.md §7.3`).

- [x] **6a.** Ajout de nouveau texte — `pdf-edit::add_free_text_annotation` construit une annotation `/FreeText` avec une apparence réelle générée (police Helvetica non intégrée, résolue par substitution système au rendu — même contrainte que `set_form_field_value`), pas seulement `/Contents`/`/DA`. Testé (`add_free_text_annotation_persists_and_renders_after_reopen`).
- [x] **6b.** Remplacement par superposition — `replace_text_with_overlay` couvre le rectangle d'un fond plein (typiquement blanc) puis dessine le nouveau texte par-dessus ; le contenu original sous-jacent n'est jamais modifié (juste recouvert). Testé (`replace_text_with_overlay_covers_without_deleting_original_content`).
- [ ] **6c. (R&D, budgété à part)** Édition chirurgicale du flux de contenu + gestion des subsets de polices — pas commencé.

**État de dépôt au moment de la rédaction :** 6a/6b sont implémentés et testés (12 tests `pdf-edit`, tous verts, dont `add_free_text_annotation_persists_and_renders_after_reopen` et `replace_text_with_overlay_covers_without_deleting_original_content`), ainsi qu'un ajout connexe `remove_annotation` (retire une référence de `/Annots`, orpheline nettoyée par `export_optimized`) — mais **ce travail n'est pas encore commité** (`git status` : `pdf-edit/src/lib.rs` modifié) et **n'apparaît pas encore dans `sprint.md`/`STATUS.md`**, qui décrivent toujours Sprint 17+ comme non démarré. À committer et à refléter dans ces deux documents avant de considérer 6a/6b réellement clos.

Règle actée (`sprint.md`) : les crates de shaping (`rustybuzz`/`swash`/`cosmic-text`, si utilisées) sont strictement réservées au layout du **nouveau texte ajouté** — jamais pour réinterpréter le texte existant, déjà géré par `pdf-core::font.rs`/`ttf-parser`. 6a/6b actuels n'utilisent aucune de ces crates (mise en page Helvetica simple, une ligne, position calculée à la main) ; à réévaluer si un layout multi-ligne/justifié devient nécessaire.

**Critère de sortie :** 6a/6b livrés et testés avant d'engager du temps sur 6c. **➜ Presque atteint côté moteur** (reste à committer/documenter) ; aucune interface `pdf-ui` pour créer une zone de texte ou déclencher un remplacement à la souris.

### 6.4 Sprints restants (non démarrés)

**Sprint 18+ — Durcissement (Épic E8)**

- [ ] Fuzzing du parser (`cargo-fuzz`) — pas encore mis en place ; à date, seule la récupération d'erreur manuelle sur fixtures corrompues existe.
- [ ] Déchiffrement `/Encrypt` (RC4/AES) — actuellement seulement détecté avec message d'erreur clair, jamais déchiffré même avec le bon mot de passe.
- [ ] Accessibilité VoiceOver, conformité PDF/A.
- [ ] Signatures numériques.

**Item de niveau 2 de conformité PDF non rattaché à un sprint dédié** (grille §2bis de STATUS.md) : CMaps CJK prédéfinis/embarqués au-delà d'`Identity-H`, Type1 historique (`/FontFile`) — à positionner dans le sprint où la valeur se présente (probablement lors d'un besoin corpus concret plutôt que de manière planifiée à l'avance).

---

## 7. Jalons produit — état réel

| Jalon | À la fin de | Ce qu'on peut montrer | État réel |
|---|---|---|---|
| M1 — Lecteur structurel | Sprint 3-4 | `pdf-cli` ouvre et décrit n'importe quel PDF | **Atteint** |
| M2 — Rendu fidèle | Sprint 7-8 | Pages affichées, conformes au pixel | **Atteint** (harnais golden en place) |
| M3 — Viewer natif | Sprint 11-12 | App macOS fluide : lire, chercher, copier | **Atteint** — chrome natif `objc2` (menus, raccourcis, mode sombre), packaging `.app`/`.dmg` fonctionnel (signature ad-hoc seulement) |
| M4 — Éditeur courant | Sprint 13-14 | Annoter, remplir, signer | **Moteur atteint, UI non atteinte** — `/Highlight`, formulaires AcroForm texte, undo/redo testés bout en bout ; aucune interaction `pdf-ui` |
| M5 — Éditeur pages ⭐ | Sprint 15-16 | Réorganiser/fusionner/extraire | **Moteur atteint, UI non atteinte** — insertion/suppression/déplacement/rotation, fusion/découpage, export optimisé testés bout en bout ; aucune interaction `pdf-ui` |
| M6 — Édition texte | Sprint 17+ | Ajouter/modifier du texte | **Entamé** — 6a (ajout `/FreeText`) et 6b (superposition) implémentés et testés côté moteur, non commités ; 6c pas commencé ; aucune UI |
| M7 — Distribuable | Sprint 18+ | `.dmg` signé et durci | **Packaging fait**, signature Developer ID/notarisation réelles pas atteintes |

⭐ **Point de décision actuel du projet :** le moteur couvre désormais tout le périmètre M3-M6 sauf 6c, mais **aucun des jalons M4/M5/M6 n'est démontrable dans l'application** — seuls `pdf-cli` et l'API Rust directe les exercent. Le prochain effort à plus forte valeur n'est plus un chantier moteur mais un chantier d'interface (`pdf-ui` : outils de surlignage/annotation à la souris, panneau de manipulation de pages, câblage undo/redo sur ⌘Z/⌘⇧Z déjà présents dans le menu) — c'est le bon moment pour confirmer avec l'utilisateur si ce chantier d'UI d'édition est prioritaire avant d'avancer 6c ou le durcissement (E8).

---

## 8. Risques & parades — mise à jour

| Risque (v1.0) | Impact | Parade prévue | État réel |
|---|---|---|---|
| Sous-estimation de l'édition texte | Élevé | Périmètre progressif, superposition d'abord | **Entamé, dans les temps** : 6a/6b (ajout + superposition) livrés au niveau moteur avec la stratégie progressive prévue, sans surprise de complexité à date. 6c (édition chirurgicale) reste le vrai risque non résorbé, toujours traité comme R&D séparée |
| Diversité des polices intégrées | Élevé | Crates de fonts matures, corpus large, spike dédié | **Largement résorbé** : TrueType, CFF/Type1C, `/Type0` (les deux sous-types CID), substitution système, `/ToUnicode` sont faits. Reste : Type1 historique (`/FontFile`), CMaps CJK non-Identity |
| Couche texte médiocre → UX cassée | Élevé | Prioriser E3 tôt, tests de sélection/recherche | **Résorbé** — sélection, recherche, surlignage fonctionnels et testés |
| PDF malformés | Moyen | Récupération xref, fuzzing, lexer tolérant | Récupération xref faite et testée ; **fuzzing (`cargo-fuzz`) pas encore mis en place** — reste un risque résiduel réel avant diffusion externe |
| Fidélité rendu (transparence, shadings) | Moyen | CPU de référence + tests pixel | `/SMask` fait, shadings/patterns pas faits (niveau 3 de la grille de conformité) |
| Complexité GUI native | Moyen | Prototype `egui`, chrome natif isolé et incrémental | **Résorbé** — le prototype `egui` a validé l'UX fonctionnelle, la bascule vers `objc2` natif est faite (Sprint 11-12, menus/raccourcis/mode sombre/packaging) |

**Risque toujours présent depuis v2.0 :** rasterisation toujours synchrone sur le thread appelant plutôt que sur un pool dédié (voir §2, invariant non respecté), **non résorbé même après l'ajout du chrome natif AppKit** — à surveiller de près maintenant que la boucle d'événements native est en place, plutôt qu'un risque théorique.

**Risque nouveau (v3.0) : dette de documentation.** Le travail de 6a/6b (Sprint 17+) existe dans le code (`pdf-edit/src/lib.rs`) sans être commité, et sans que `sprint.md`/`STATUS.md` en fassent état — si ce travail est perdu ou divergent, la trace du « pourquoi » disparaît avec lui. Committer et documenter avant de poursuivre.

**Risque nouveau (v3.0) : écart moteur/UI qui se creuse.** Trois sprints consécutifs (13-14, 15-16, 17+) ont livré du moteur d'édition sans UI correspondante. Chaque sprint supplémentaire dans cette direction élargit le rattrapage `pdf-ui` à faire d'un coup — à surveiller pour éviter un « gros bang » d'intégration UI en fin de parcours plutôt qu'un travail incrémental.

---

## 9. Décisions Sprint 0 — statut de résolution

| Décision à prendre (v1.0 §9) | Statut |
|---|---|
| Confirmer la frontière maison vs crates | **Tranchée par la pratique** — voir §1/§2 ci-dessus |
| Trancher le framework GUI (egui prototype → natif ? Iced ? Slint ?) | **Tranchée et exécutée** : `egui` retenu comme moteur de rendu de l'UI (conservé), chrome natif via `objc2` livré par-dessus (Sprint 11-12) — pas de bascule complète hors `egui`, mais le chrome système (menus, mode sombre) passe bien par AppKit natif |
| Fixer le seuil d'écart pixel acceptable | **Tranchée et implémentée** — `pdf-render/tests/golden.rs`, seuil configurable, `UPDATE_GOLDEN=1` pour régénérer |
| Définir la vélocité cible initiale | **Non instrumentée formellement** (pas d'outil de suivi de points externe visible dans ce dépôt) |
| Cibles de compatibilité (PDF 1.7 seul ? 2.0 ? chiffrés ? CJK ?) | **De facto couvert très largement** : PDF avec xref classique et streams, CJK (`/Type0`/CID des deux sous-types), chiffrement **détecté mais pas déchiffré** (à trancher explicitement si le déchiffrement doit être priorisé avant Sprint 18+) |
| Ouvrir le compte développeur Apple | **Toujours non disponible dans cet environnement de développement** — le Sprint 11-12 a livré le packaging `.app`/`.dmg` avec une signature ad-hoc seulement (linker), sans signature Developer ID ni notarisation réelle ; reste bloquant pour toute diffusion externe |

---

## 10. Ce qu'il faut décider maintenant (au lieu de Sprint 0)

Contrairement à v1.0 où ces décisions précédaient tout code, elles se posent maintenant **avec le bénéfice de seize sprints de réalité** :

1. **Committer et documenter le travail 6a/6b en cours** (`pdf-edit/src/lib.rs`, non commité) — mettre à jour `sprint.md`/`STATUS.md` en conséquence avant de poursuivre, pour ne pas perdre la trace du « pourquoi » de ces choix d'implémentation.
2. **Prioriser l'interface `pdf-ui` pour l'édition (E5/E6, déjà livrés côté moteur) avant de poursuivre 6c ou le durcissement (E8) ?** C'est désormais la question la plus structurante : trois sprints de moteur d'édition sans UI correspondante ont créé un écart qui, laissé filer, se traduira par un gros chantier d'intégration en une fois plutôt qu'incrémental (voir risque §8).
3. **Confirmer le statut du compte développeur Apple** — toujours non résolu, bloquant pour la signature Developer ID/notarisation réelles (le `.dmg` actuel n'a qu'une signature ad-hoc).
4. **Décider si le déchiffrement `/Encrypt` (RC4/AES) doit être avancé** avant Sprint 18+, si des PDF chiffrés réels sont dans le périmètre d'usage à court terme.
5. **Décider si la migration vers un pool de threads pour la rasterisation** doit être traitée maintenant que le chrome natif AppKit est en place (Sprint 11-12) — le risque identifié avant ce sprint ne s'est pas encore matérialisé en bug connu, mais reste non vérifié en usage réel.
6. **Confirmer que le corpus de test actuel (25 fixtures, diversité structurelle) est suffisant** pour continuer, ou si un effort dédié pour obtenir un corpus de *volume* (centaines de PDF réels/scans/PDF-A tiers) doit être engagé — actuellement noté comme hors de portée de l'environnement de développement en l'état.

---

*Ce document complète — sans les remplacer — [architecture.md](./architecture.md) (la cible), [STATUS.md](./STATUS.md) (l'état vérifiable ligne par ligne) et [sprint.md](./sprint.md) (le plan de sprints détaillé). En cas de divergence future entre ce document et le code, ces trois fichiers font foi.*
