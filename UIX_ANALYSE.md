# UIX Analyse — Dossier de conception & plan d'exécution (mis à jour à l'état réel du projet)

Éditeur / Visionneuse PDF natif macOS — Rust, moteur maison

**Version :** 2.0 (révision à partir de l'état réel du code)
**Date :** 2026-07-04
**Destinataire :** développeur(euse) Rust en charge de la réalisation
**Statut :** projet en cours — Phases 0 à 3 (viewer) très largement avancées, chrome natif macOS et édition restent à faire

> Ce document reprend et met à jour la version 1.0 du dossier de conception initial, en la confrontant à l'état réel du dépôt (voir [STATUS.md](./STATUS.md) pour le détail vérifiable commande par commande, et [sprint.md](./sprint.md) pour le suivi sprint par sprint). Les sections ci-dessous ne dupliquent pas ces deux fichiers : elles renvoient vers eux et signalent explicitement les écarts entre le plan initial et la réalité.

---

## 1. Résumé exécutif

Construire une application native macOS (`.dmg`) en Rust, avec un moteur PDF écrit de zéro pour la logique PDF (parsing, modèle document, interprétation de contenu, résolution de polices), permettant de visualiser, annoter, manipuler les pages et éditer le contenu de PDF.

Décisions actées et **confirmées par le code existant** :

| Sujet | Décision v1.0 | État réel |
|---|---|---|
| Plateforme | macOS natif, `.dmg` signé/notarisé | Pas encore commencé : le binaire `pdf-ui` tourne dans une fenêtre `egui`/`eframe` générique, sans packaging `.app`/`.dmg`, sans signature ni notarisation (Sprint 11-12, non démarré) |
| Moteur | Maison (logique PDF from scratch) | **Confirmé et livré** : lexer, xref, filtres, modèle document, interpréteur de contenu, résolution de polices sont bien maison (`pdf-core`), sur ~40 opérateurs de contenu, polices simples et composites `/Type0`/CID |
| Frontière maison / crates | À confirmer en Sprint 0 | **Tranchée par la pratique** : codecs génériques (`flate2` implicite via filtres maison en fait écrits à la main, `zune-jpeg` pour JPEG), rasterisation (`tiny-skia` CPU, `wgpu`+`lyon` GPU), police bas niveau (`ttf-parser`), GUI (`egui`/`eframe`, `rfd` pour les dialogues) — la logique PDF elle-même reste 100 % maison |
| Édition texte | Cible « complète », exécution progressive (superposition d'abord) | **Pas commencé** : `pdf-edit` est un stub vide. Reste prévu en périmètre progressif (Sprint 17+, voir §6) |
| Livrable | Documents Markdown versionnés avec le code | Confirmé : `architecture.md`, `STATUS.md`, `sprint.md`, et ce document |

**Réalité à garder en tête (inchangée depuis v1.0) :** le moteur maison n'apporte pas d'avantage de vitesse sur PDFium/MuPDF. La valeur du produit se joue sur l'UX native et la maîtrise complète du code. **Constat après ~10 sprints de développement réel :** cette hypothèse se vérifie — le moteur de rendu (CPU et GPU) est aujourd'hui solide et testé (169 tests, harnais de comparaison pixel sur 23 fixtures), mais **l'écart le plus visible avec un « vrai » produit macOS reste l'absence de chrome natif** (menus système, raccourcis, packaging). C'est la prochaine étape qui matérialise le pari du produit.

---

## 2. Architecture — état réel du workspace

Le workspace Cargo comporte aujourd'hui 8 crates (tous membres, tous compilent, aucun n'est un placeholder sauf `pdf-edit`) :

| Crate | Rôle prévu (v1.0) | État réel |
|---|---|---|
| `pdf-core` | Lexer, objets/xref, filtres, modèle document, interpréteur de contenu, polices | **Fait**, y compris récupération d'erreur, object streams, polices composites `/Type0`/CID (`CIDFontType0` et `CIDFontType2`), `/ToUnicode`, `/Outlines` |
| `pdf-render` | Rasterisation CPU (référence) | **Fait** — `tiny-skia`, clip réel (`W`/`W*`), rotation, images avec alpha, harnais golden (`tests/golden.rs`, 23 fixtures) |
| `pdf-render-gpu` | Rasterisation GPU (wgpu/Metal) | **Fait, en parité fonctionnelle avec le CPU** — `wgpu` + `lyon`, stencil buffer pour le clip, cache de glyphes, branché dans `pdf-ui` avec repli automatique sur CPU si pas d'adaptateur. Comparaison pixel CPU/GPU automatisée (`tests/cross_backend.rs`, 12 fixtures) |
| `pdf-text` | Extraction, sélection, recherche | **Fait** pour l'extraction linéaire, la recherche avec surlignage, la sélection (hit-test caractère). Pas de reconstruction par blocs/colonnes |
| `pdf-edit` | Opérations d'édition, journal, undo/redo, sauvegarde incrémentale | **Stub vide** — rien d'implémenté, correspond aux Sprints 13-14 et 17+ non démarrés |
| `pdf-app` | État applicatif, contrôleur, orchestration | **Fait** — `Session` porte document, navigation, rendu (avec cache FIFO 32 entrées), recherche, sélection, table des matières (chacun avec son propre cache). Pas d'undo/redo (attend `pdf-edit`), pas de multi-documents |
| `pdf-ui` | Interface macOS (cible : rendu wgpu + chrome natif AppKit via `objc2`) | **Prototype fonctionnel mais non natif** — fenêtre `egui`/`eframe`, dialogues de fichiers natifs via `rfd`, mais **aucun menu système, aucun raccourci hors ⌘C, aucun mode sombre système, aucun packaging** |
| `pdf-cli` | Outil debug/tests/batch | **Fait** — `dump`, `render`, `render-info`, `text` |

**Décision de dépendance actée pour le chrome natif** (`sprint.md` Sprint 11-12) : `objc2` + crates officielles par framework (`objc2-foundation`, `objc2-app-kit`, `objc2-quartz-core`) plutôt que `cacao`, pour ne pas mélanger deux couches de bindings Objective-C. **Ce choix reste à exécuter**, `pdf-ui` n'a pas encore commencé sa migration hors `egui`.

Invariants de conception (v1.0) — état de mise en œuvre :

- Document chargé immuable ; modifications = journal d'opérations → **pas encore implémenté** (`pdf-edit` vide, aucun `EditOp`).
- Rasterisation hors du thread UI → **pas encore fait** ; le rendu est aujourd'hui synchrone dans `pdf-app::Session` (mis en cache, mais pas sur un pool de threads séparé). À vérifier avant Sprint 11-12 si la fluidité observée avec `egui` reste acceptable une fois le chrome natif ajouté.
- Résolution paresseuse des objets → **fait** (cache de résolution dans `pdf-core::document`).
- Parser tolérant aux fichiers malformés → **fait** (reconstruction xref par balayage d'octets, repli sur recherche `/Type /Catalog`).
- Zéro `unsafe` non justifié → à auditer explicitement (pas de revue dédiée à date dans ce document).

---

## 3. UX cible — état réel face au MoSCoW de v1.0

| Priorité | Item | État réel |
|---|---|---|
| **Must** | Scroll/zoom fluide | Fait (`egui`, molette+Ctrl, pincement trackpad, scroll continu virtualisé) |
| **Must** | Miniatures | Fait (panneau latéral, cache) |
| **Must** | Menus & raccourcis natifs | **Pas fait** — aucun menu système, seul ⌘C est géré |
| **Must** | Mode sombre | **Pas fait** (ni chrome, ni option de fond de page) |
| **Must** | Sélection + copier texte | Fait (glisser-souris, ⌘C, bouton), limité au mode page unique |
| **Must** | Recherche ⌘F | Fait au niveau fonctionnel (recherche + surlignage), mais **pas de raccourci ⌘F** câblé (pas de menu/accélérateurs natifs) |
| **Must** | Annotation + surlignage | **Pas fait** (`pdf-edit` vide) |
| **Must** | Réorganisation de pages | **Pas fait** |
| **Must** | Undo/redo | **Pas fait** |
| **Must** | Ouverture paresseuse | Fait (résolution paresseuse des objets) |
| **Must** | Navigation clavier | Partielle — pas de tab order/accessibilité de base formalisée |
| **Should** | Onglets natifs | Pas fait |
| **Should** | Quick Look | Pas fait |
| **Should** | Édition texte par superposition | Pas fait (périmètre Sprint 17+) |
| **Should** | Inspecteur | Pas fait |
| **Should** | Fusion/split | Pas fait |
| **Should** | VoiceOver | Pas fait |
| **Could** | Zoom sémantique, Continuity, Spotlight, iCloud | Pas fait |

**Constat :** sur l'axe « viewer », la majorité des Must UX sont couverts au niveau fonctionnel (rendu, navigation, recherche, sélection). Ce qui manque structurellement, ce sont les Must **d'intégration système macOS** (menus, raccourcis, mode sombre, packaging) — c'est le écart entre « bon prototype » et « app macOS qui se sent native », exactement le risque identifié en v1.0 §8 (« Complexité GUI native »).

---

## 4. Épics — état réel

| # | Épic | Valeur | Dépend de | État réel |
|---|---|---|---|---|
| E0 | Fondations & outillage | Base saine, CI, corpus de test | — | **Fait** (25 fixtures, CI fmt/clippy/tests, harnais golden pixel) |
| E1 | Parser & modèle document | Ouvrir/lire n'importe quel PDF | E0 | **Fait** (xref classique + streams, object streams, récupération d'erreur, filtres majeurs) |
| E2 | Rendu (viewer visuel) | Afficher fidèlement une page | E1 | **Fait** (CPU + GPU en parité, polices intégrées + substituées + composites, images avec alpha, clip, rotation) |
| E3 | Couche texte | Sélection, copier, recherche | E2 | **Fait** (extraction linéaire, sélection, recherche avec surlignage) |
| E4 | UX viewer natif macOS | Produit démontrable en lecture | E2, E3 | **Partiel** — riche fonctionnellement (miniatures, signets, scroll continu, zoom, GPU) mais **pas natif** (pas de chrome AppKit, pas de packaging) — c'est le travail restant du Sprint 11-12 |
| E5 | Annotations & formulaires | « Éditeur » utile | E4 | **Pas commencé** |
| E6 | Manipulation de pages | Réorganiser/fusionner/split | E1, E4 | **Pas commencé** |
| E7 | Édition de texte (progressive) | Modifier le contenu | E3, E5 | **Pas commencé** |
| E8 | Durcissement & distribution | Robustesse, `.dmg`, sécurité | tous | **Pas commencé** (chiffrement détecté mais non déchiffré ; pas de fuzzing formalisé au-delà du corpus de corruption manuelle) |

**Chemin critique révisé :** E0 → E1 → E2 → E3 sont clos. **Le blocage actuel du chemin critique est E4** (chrome natif), pas le moteur — contrairement à la situation de départ où le risque principal perçu portait sur le moteur de rendu.

---

## 5. Cadre agile & re-planification

Le cadre décrit en v1.0 (sprints de 2 semaines, story points Fibonacci, Definition of Ready/Done, cérémonies, phase gates par épic, règles de re-priorisation, spikes, budget de risque pour l'édition texte) **reste valide sans changement** et n'est pas reproduit ici in extenso — voir la version 1.0 conservée dans l'historique, ou considérer les règles suivantes comme actives :

- Phase gate à la fin de chaque épic — **appliquée dans les faits** : `sprint.md` marque explicitement chaque sprint « Statut réel » avec les réserves restantes plutôt que de le clore artificiellement (ex. Sprint 3-4 : critère « plusieurs centaines de PDF » non atteint littéralement, mais clos au sens fonctionnel).
- Trigger de re-découpe à 150 % d'estimation — pas d'historique de vélocité chiffré disponible dans ce dépôt ; à instrumenter si un outil de suivi externe est mis en place.
- Budget de risque sur l'édition texte (E7) — toujours d'actualité, non entamé.

**Tableau de bord actuel (constats extraits de STATUS.md) :**

- 169 tests automatisés, tous verts (`cargo test --workspace`).
- `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre.
- Corpus : 25 fixtures (diversité structurelle large, pas encore de volume — voir §4 de STATUS.md).
- Écart pixel : harnais en place (`pdf-render/tests/golden.rs`, seuil configurable), 23 fixtures comparées.
- Bugs réels trouvés et corrigés en cours de route : `/Rotate` ignoré au rendu, message d'erreur trompeur sur PDF chiffré, décodage JPEG CMYK cassé — signe que le corpus élargi remplit son rôle.

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

**Bilan des sprints clos :** couvrent la quasi-totalité des épics E0-E3 et une large partie fonctionnelle de E4. C'est un viewer PDF complet et testé, qui n'est pas encore une application macOS packagée.

### 6.2 Sprint en cours / prochain — Sprint 11-12 (chrome natif & packaging, Épic E4)

**Objectif :** premier produit démontrable en tant qu'app macOS.

- [ ] Chrome natif macOS via `objc2` (+ `objc2-foundation`, `objc2-app-kit`, `objc2-quartz-core`) : menus système, raccourcis (⌘O/⌘S/⌘Z/⌘⇧Z/⌘F/⌘C confirmés), glisser-déposer, plein écran, mode sombre.
- [ ] Ouverture/sauvegarde natives (`NSOpenPanel`/`NSSavePanel` — remplace ou complète `rfd`), Quick Look.
- [ ] Packaging `.dmg` : `cargo-bundle`, `codesign`, `notarytool`.
- [ ] Point d'attention à traiter dans ce sprint : décider si la rasterisation doit passer sur un pool de threads dédié avant d'intégrer le chrome natif (actuellement synchrone dans `pdf-app::Session`), pour éviter toute régression de fluidité perçue.

**Critères de sortie (fin Phase 3) :** `.dmg` signé/notarisé installable, application démontrable en interne. **➜ Jalon : premier produit réellement démontrable comme app macOS.**

**Prérequis externe non technique :** compte développeur Apple pour la signature/notarisation — statut à vérifier avec l'utilisateur avant ce sprint (item ouvert depuis le §9 « décisions Sprint 0 » de v1.0, jamais explicitement confirmé dans le dépôt).

### 6.3 Sprints restants (non démarrés)

**Sprint 13-14 — Annotations & formulaires (Épic E5)**

Prérequis technique signalé dans `sprint.md` (à faire au moment d'attaquer ce sprint, pas avant) : migrer `Object::Array`/`Object::Dictionary` vers des types partagés (`Arc<[Object]>`, `Arc<Dictionary>`) et ajouter un `SourceSpan` par objet indirect, utile pour la sauvegarde incrémentale et l'édition structurelle.

- [ ] Annotations : surlignage, notes, formes, texte libre, signatures.
- [ ] Remplissage de formulaires AcroForm.
- [ ] Journal d'opérations (`EditOp`) + undo/redo.
- [ ] Sauvegarde incrémentale (append xref).

Critère de sortie : annotations et formulaires persistés correctement après réouverture. **➜ Jalon : éditeur utile au quotidien.**

**Sprint 15-16 — Manipulation de pages (Épic E6)**

- [ ] Insérer / supprimer / déplacer / pivoter des pages.
- [ ] Fusion et découpage de documents.
- [ ] Insertion d'images et de pages depuis d'autres PDF.
- [ ] Export / optimisation (linéarisation, garbage collection des objets orphelins).

Critère de sortie : toutes les opérations validées sur le corpus, sans corruption de document.

**Sprint 17+ — Édition de texte, périmètre progressif (Épic E7)**

- [ ] 6a. Ajout de nouveau texte (annotations FreeText gérées par l'éditeur).
- [ ] 6b. Remplacement par superposition d'un texte existant (masquer l'ancien + redessiner).
- [ ] 6c. (R&D, budgété à part) Édition chirurgicale du flux de contenu + gestion des subsets de polices.

Règle actée (`sprint.md`) : les crates de shaping (`rustybuzz`/`swash`/`cosmic-text`, si utilisées) sont strictement réservées au layout du **nouveau texte ajouté** — jamais pour réinterpréter le texte existant, déjà géré par `pdf-core::font.rs`/`ttf-parser`.

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
| M3 — Viewer natif ⭐ | Sprint 11-12 | App macOS fluide : lire, chercher, copier | **Fonctionnellement atteint, pas nativement** — `pdf-ui` fait tout ça dans une fenêtre `egui` ; le jalon « natif » au sens propre (menus, raccourcis, packaging) reste à faire |
| M4 — Éditeur courant | Sprint 13-14 | Annoter, remplir, signer | Pas atteint |
| M5 — Éditeur pages | Sprint 15-16 | Réorganiser/fusionner/extraire | Pas atteint |
| M6 — Édition texte | Sprint 17+ | Ajouter/modifier du texte | Pas atteint |
| M7 — Distribuable | Sprint 18+ | `.dmg` signé et durci | Pas atteint |

⭐ **Point de décision actuel du projet :** le contenu fonctionnel de M3 est déjà démontrable, mais pas sous une forme qui « se sent » native macOS. Le prochain effort (Sprint 11-12) est donc moins un risque technique nouveau qu'un chantier d'intégration système bien identifié (`objc2`) — c'est le bon moment pour confirmer avec l'utilisateur si ce chantier de chrome natif est prioritaire avant d'attaquer l'édition (E5-E7), conformément à l'ordre du backlog.

---

## 8. Risques & parades — mise à jour

| Risque (v1.0) | Impact | Parade prévue | État réel |
|---|---|---|---|
| Sous-estimation de l'édition texte | Élevé | Périmètre progressif, superposition d'abord | Toujours à venir — pas encore d'information nouvelle car E7 non démarré |
| Diversité des polices intégrées | Élevé | Crates de fonts matures, corpus large, spike dédié | **Largement résorbé** : TrueType, CFF/Type1C, `/Type0` (les deux sous-types CID), substitution système, `/ToUnicode` sont faits. Reste : Type1 historique (`/FontFile`), CMaps CJK non-Identity |
| Couche texte médiocre → UX cassée | Élevé | Prioriser E3 tôt, tests de sélection/recherche | **Résorbé** — sélection, recherche, surlignage fonctionnels et testés |
| PDF malformés | Moyen | Récupération xref, fuzzing, lexer tolérant | Récupération xref faite et testée ; **fuzzing (`cargo-fuzz`) pas encore mis en place** — reste un risque résiduel réel avant diffusion externe |
| Fidélité rendu (transparence, shadings) | Moyen | CPU de référence + tests pixel | `/SMask` fait, shadings/patterns pas faits (niveau 3 de la grille de conformité) |
| Complexité GUI native | Moyen | Prototype `egui`, chrome natif isolé et incrémental | **C'est le risque actif du moment** — le prototype `egui` a rempli son rôle (valider l'UX fonctionnelle), la bascule vers `objc2` natif reste entièrement à faire et n'a pas encore été spikée |

**Risque nouveau non présent en v1.0 :** rasterisation actuellement synchrone sur le thread appelant plutôt que sur un pool dédié (voir §2, invariant non respecté) — à surveiller quand le chrome natif ajoutera de la pression sur la boucle d'événements AppKit.

---

## 9. Décisions Sprint 0 — statut de résolution

| Décision à prendre (v1.0 §9) | Statut |
|---|---|
| Confirmer la frontière maison vs crates | **Tranchée par la pratique** — voir §1/§2 ci-dessus |
| Trancher le framework GUI (egui prototype → natif ? Iced ? Slint ?) | **Partiellement tranchée** : `egui` retenu comme prototype (fait), cible finale confirmée `objc2` natif pour le chrome (décision dans `sprint.md` Sprint 11-12) — reste à exécuter |
| Fixer le seuil d'écart pixel acceptable | **Tranchée et implémentée** — `pdf-render/tests/golden.rs`, seuil configurable, `UPDATE_GOLDEN=1` pour régénérer |
| Définir la vélocité cible initiale | **Non instrumentée formellement** (pas d'outil de suivi de points externe visible dans ce dépôt) |
| Cibles de compatibilité (PDF 1.7 seul ? 2.0 ? chiffrés ? CJK ?) | **De facto couvert très largement** : PDF avec xref classique et streams, CJK (`/Type0`/CID des deux sous-types), chiffrement **détecté mais pas déchiffré** (à trancher explicitement si le déchiffrement doit être priorisé avant Sprint 18+) |
| Ouvrir le compte développeur Apple | **Statut inconnu dans ce dépôt — à confirmer avec l'utilisateur avant le Sprint 11-12**, condition bloquante pour la notarisation `.dmg` |

---

## 10. Ce qu'il faut décider maintenant (au lieu de Sprint 0)

Contrairement à v1.0 où ces décisions précédaient tout code, elles se posent maintenant **avec le bénéfice de dix sprints de réalité** :

1. **Prioriser le chrome natif (E4/Sprint 11-12) avant l'édition (E5+) ?** C'est l'ordre du backlog actuel et il reste cohérent : sans packaging natif, aucun jalon suivant n'est « distribuable » même en interne.
2. **Confirmer le statut du compte développeur Apple** — bloquant pour la notarisation, à vérifier avant d'investir dans le Sprint 11-12.
3. **Décider si le déchiffrement `/Encrypt` (RC4/AES) doit être avancé** avant Sprint 18+, si des PDF chiffrés réels sont dans le périmètre d'usage à court terme.
4. **Décider si la migration vers un pool de threads pour la rasterisation** doit précéder ou accompagner le travail de chrome natif (Sprint 11-12), pour éviter une régression de fluidité en environnement AppKit réel.
5. **Confirmer que le corpus de test actuel (25 fixtures, diversité structurelle) est suffisant** pour continuer, ou si un effort dédié pour obtenir un corpus de *volume* (centaines de PDF réels/scans/PDF-A tiers) doit être engagé — actuellement noté comme hors de portée de l'environnement de développement en l'état.

---

*Ce document complète — sans les remplacer — [architecture.md](./architecture.md) (la cible), [STATUS.md](./STATUS.md) (l'état vérifiable ligne par ligne) et [sprint.md](./sprint.md) (le plan de sprints détaillé). En cas de divergence future entre ce document et le code, ces trois fichiers font foi.*
