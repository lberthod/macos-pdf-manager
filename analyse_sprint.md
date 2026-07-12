# Analyse & plan de sprints — combler les trous de l'audit 50 fonctionnalités

**Source :** [audit50quest.md](./audit50quest.md) (couverture Must ≈ 54 %).
**Objectif de ce document :** transformer l'écart constaté en sprints actionnables, dans la continuité de la numérotation déjà utilisée dans [sprint.md](./sprint.md) (le projet en est au Sprint 17+ / note de suivi post-17+). Chaque sprint ci-dessous part d'un constat précis de l'audit (n° de ligne) et d'une preuve code, pas d'une supposition.

**Constat transverse qui structure tout ce plan :** le moteur (`pdf-core`/`pdf-render`/`pdf-render-gpu`/`pdf-text`/`pdf-edit`) est largement en avance sur l'interface (`pdf-ui`). Sur les 24 lignes Must de l'audit, 14 sont "◐ partiel" — et dans la moitié de ces cas, le partiel signifie *"le moteur marche, zéro UI"* (groupe F en entier, une bonne partie du groupe G). Le levier de valeur le plus élevé n'est donc pas d'écrire plus de moteur, mais de **câbler l'existant à l'interface**.

---

## Sprint 18 — Navigation & confort de base (Must, effort faible) — ✅ FERMÉ

**Pourquoi en premier :** ce sont les manques Must les moins chers à corriger et les plus visibles pour n'importe quel utilisateur, avant même de toucher à l'édition.

- [x] **#3 — Aller directement à une page.** Champ de saisie dans la barre d'outils (`⌘G` pour lui donner le focus) → `Session::goto_page`, déjà borné. `pdf-ui/src/main.rs::goto_page_from_input`.
- [x] **#25 — Export du texte en .txt.** Nouvelle méthode `pdf_app::Session::extract_all_text` (tout le document, un saut de page `\x0c` par page, réutilise le cache de texte existant) + bouton "📄 Exporter le texte…" (`NSSavePanel` via `rfd`).
- [x] **#22 — Recherche insensible aux accents.** `pdf_text::fold_char_for_search`/`normalize_for_search` (décomposition NFD via la crate `unicode-normalization`, ne garde que le caractère de base avant minuscule) — remplace le `to_ascii_lowercase` de `PageText::find_matches` et le `to_lowercase` de `Session::find_pages_containing`.
- [x] **#23 — Double-clic (mot) / triple-clic (ligne).** `PageText::word_range_at`/`line_range_at` (nouvelles méthodes, bornes alphanumériques/saut de ligne) exposées via `Session`, câblées dans `handle_text_selection` (`response.double_clicked()`/`triple_clicked()`).

**Critère de sortie : atteint.** `cargo test --workspace` vert (200 tests, +5 nouveaux), `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre, lancement réel de `pdf-ui` sur un fixture sans panique. Détail dans [sprint.md](./sprint.md#sprint-18--navigation--confort-de-base) ; grille mise à jour dans [audit50quest.md](./audit50quest.md) (couverture Must 54 % → 58 %).

---

## Sprint 19 — Câbler la manipulation de pages à `pdf-ui` (Must + Should, groupe F) — ✅ FERMÉ

**Pourquoi maintenant :** c'est le point isolé par l'audit comme "angle mort supplémentaire" — 6 fonctionnalités (#33-38) sont **entièrement faites côté moteur** (testées bout en bout dans `pdf-edit`) et **entièrement absentes côté UI**. C'est le ratio valeur/effort le plus favorable de tout ce plan : aucune nouvelle logique métier à écrire, seulement de l'intégration.

- [x] **#5 + #33 — Glisser-déposer dans le panneau miniatures** pour réordonner — `egui::Ui::dnd_drag_source`/`dnd_drop_zone` (API intégrée `egui` 0.24+, pas besoin d'une crate externe) dans `show_thumbnail_panel` → `Session::move_page` (déjà testé côté moteur).
- [x] **#34 — Supprimer une page** : bouton 🗑 sur chaque miniature → `Session::delete_page_at` (undo déjà câblé depuis le Sprint 18-suivi, réutilisé tel quel).
- [x] **#35 — Pivoter une page** : bouton ↻ (+90°) sur chaque miniature → `Session::rotate_page_at`.
- [x] **#36 — Insérer une page** : boutons "＋ Page" (vierge, `MediaBox` de la page courante) et "🖼 Image…" (`NSOpenPanel` filtré JPEG) → `insert_blank_page_at`/`insert_image_page_at`.
- [x] **#37 — Fusionner un PDF** : bouton "📎 PDF…" (`NSOpenPanel`) → `Session::merge_document_from_path`.
- [x] **#38 — Découper/extraire + sélection multiple** : case à cocher par miniature (`thumbnail_selection: HashSet<usize>`) + bouton "✂ Extraire (N)…" (`NSSavePanel`) → `Session::extract_pages_to_file`.
- [ ] **#11 — Rotation d'affichage (vue, non persistée)** — **volontairement pas traité dans ce sprint** : reste un point à trancher avec le produit (voir "Points à trancher" plus bas) avant d'ajouter une deuxième notion de rotation à côté de #35.

**Risque anticipé vs réel :** comme prévu, aucune opération de ce sprint n'a touché `pdf-core`/`pdf-edit` — uniquement une nouvelle couche de wrappers fins dans `pdf_app::Session` (même schéma que `add_highlight_on_current_page`) et l'intégration `egui` côté `pdf-ui`. Le glisser-déposer (`dnd_drag_source`/`dnd_drop_zone`) a compilé et fonctionné du premier coup, y compris avec des boutons interactifs (🗑/↻/case à cocher) imbriqués dans la zone de glissement — aucun conflit de capture d'événements rencontré.

**Critère de sortie : atteint.** Les 6 lignes du groupe F passent de ◐ à ☑ (`audit50quest.md`). `cargo test --workspace` vert (206 tests, +6 nouveaux dans `pdf-app`), `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre, lancement réel de `pdf-ui` sur un document de 60 pages sans panique.

---

## Sprint 20 — Câbler l'annotation & l'édition de texte à `pdf-ui` (Should, groupe E + G) — ✅ FERMÉ (#43 fermé séparément, voir Sprint 50)

**Pourquoi ensuite :** même diagnostic que le Sprint 19 (moteur fait, UI absente) mais sur un périmètre plus délicat côté ergonomie (placement au clic, dialogues de saisie).

- [x] **#30 — Outil FreeText** : bouton bascule "📝 Ajouter texte", clic sur la page (`handle_add_text_click`) → dialogue de saisie (`egui::Window` modal, `show_text_modal`) → `Session::add_free_text_on_current_page` avec les coordonnées du clic.
- [x] **#40 (UI) — Remplacer un texte existant** : réutilise la sélection de texte déjà existante (`selection_bbox`, extrait de `highlight_selection`) pour proposer "✏ Remplacer…" → dialogue de saisie préremplie → `Session::replace_text_on_current_page` (nouveau wrapper).
- [x] **#43 (UI) — Remplissage de formulaire au clic** : reporté au Sprint 20, fermé hors sprint dédié juste après (voir `sprint.md` Sprint 50) — `EditSession::form_fields` + wrappers `pdf-app` + contour cliquable/dialogue préremplie dans `pdf-ui`. Champs `/Tx` seulement, `/Btn`/`/Ch` restent hors périmètre.
- [x] **#32 — Sélection/suppression d'annotation** : `Session::annotations_on_current_page` (nouvelle méthode, liste `/Annots` avec `Rect`/`Subtype`) → contour cliquable par annotation (`draw_annotation_outlines`) → clic (`handle_annotation_click`) → bouton "🗑 Supprimer l'annotation" → `remove_annotation_on_current_page` (déjà existant). Le déplacement/redimensionnement (poignées) et le réglage couleur/opacité restent **volontairement différés** — la ligne reste ◐, pas ☑, dans `audit50quest.md`.
- [x] **#26 — Souligner/barrer** : `pdf_edit::EditSession::add_underline_annotation`/`add_strikeout_annotation` (nouveau, partagent `add_line_markup_annotation` — une ligne tracée à `line_y_fraction` de la hauteur du rectangle plutôt que le remplissage semi-transparent de `/Highlight`), boutons "Souligner"/"Barrer" à côté de "🖍 Surligner" dans `pdf-ui`.

**Risque principal, confirmé après coup :** #43 (formulaire au clic) demandait bien une vraie extension moteur (localisation des widgets), contrairement au reste de ce sprint qui n'était que de l'intégration UI — isolé comme prévu, reporté sans bloquer le reste.

**Critère de sortie : atteint pour #26/#30/#32(partiel)/#40.** `cargo test --workspace` vert (209 tests, +1 `pdf-edit` +2 `pdf-app`), `cargo clippy --workspace --all-targets` sans avertissement, `cargo fmt --check` propre, lancement réel de `pdf-ui` sans panique. #43 confirmé hors périmètre de ce sprint, à traiter séparément (voir `sprint.md` Sprint 20 pour le détail).

---

## Sprint 21 — Durcissement viewer (Must, groupe C/I) — ✅ FERMÉ (sous réserve de #48)

**Pourquoi ici et pas avant :** ces items sont Must mais demandent un vrai travail moteur/systèm (pas juste du câblage UI), donc plus longs — les séquencer après les sprints à fort ratio valeur/effort ci-dessus est cohérent avec la logique "value/effort" déjà utilisée dans `sprint.md` §5.

- [x] **#48 — Impression.** Approche (b) retenue comme recommandé : `Session::current_bytes` (nouveau) écrit le document courant vers un fichier temporaire, puis `ViewerApp::print_document` délègue à Aperçu via AppleScript (`osascript`, `print ... with properties {print dialog:true}`) — "Imprimer…" (`⌘P` + bouton). **Non vérifié interactivement** (pas d'accès GUI macOS depuis cet environnement de développement pour confirmer que le dialogue s'ouvre réellement) — reste ◐ dans l'audit tant que ce n'est pas testé manuellement.
- [x] **#20 — Rendu hors thread UI.** Nouveau module `pdf-ui/src/render_worker.rs` : thread dédié + canaux MPSC, reparse son propre `pdf_core::Document` depuis les octets courants (`Session::current_bytes`, réutilisé), rendu CPU uniquement (`pdf-render`, pas de partage GPU entre threads). Couvre miniatures et défilement continu (le vrai point de blocage sur gros document) ; la page unique reste synchrone (délibéré).
- [x] **#9 — Pincement centré sur le curseur.** Décalage de défilement recalculé à chaque pincement à partir de `ScrollAreaOutput` de la frame précédente (`last_scroll_offset`/`last_scroll_viewport`), appliqué via `ScrollArea::scroll_offset` à la frame suivante.
- [x] **#10 — Fit-to-page et taille réelle.** `fit_to_page` (nouveau, symétrique de `fit_to_width`) + bouton "↕ Ajuster à la page" ; "Taille réelle (100 %)" clarifiée sur le bouton "Réinitialiser" existant plutôt que dupliquée.

**Écart avec le plan initial :** l'option (a) (`NSPrintOperation` natif) n'a pas été retenue, conformément à la recommandation — l'option (b) était bien la moins chère et préserve l'expérience native. Pour #20, le tuilage complet (dalles indépendantes d'une page) n'a pas été fait, seul le déplacement du calcul hors du thread UI — c'était déjà annoncé comme le minimum Must dans le plan initial.

**Critère de sortie : atteint pour #9/#10/#20** (☑ dans `audit50quest.md`) ; **#48 implémenté mais non vérifié** (◐), à confirmer manuellement avant de le considérer clos. `cargo test --workspace` vert (209 tests, sans régression), `cargo clippy`/`cargo fmt` propres, lancement réel sur `large_60_pages.pdf` sans geler ni paniquer.

---

## Sprint 22 — Intégration système restante (Should/Must, groupe I/J) — ✅ FERMÉ (sauf #49/accessibilité, arbitrés)

- [x] **#1 (partiel) — Association de fichier Finder.** `CFBundleDocumentTypes` ajouté via `osx_info_plist_exts` (`cargo-bundle` n'a pas de champ dédié, mais fusionne tel quel un fichier plist externe — `pdf-ui/document_types.plist.xml`). Vérifié réellement (`cargo bundle` + `plutil -lint`/`-extract`). **Bonus découvert en vérifiant** : le chemin de l'icône (`icon = [...]`) était résolu relatif au répertoire d'*invocation* de `cargo bundle`, pas au crate — l'icône n'était donc jamais réellement embarquée avec la commande documentée (lancée depuis la racine du workspace) ; corrigé du même coup. **Non fait** : `NSApplicationDelegate::application:openURLs:` (double-clic → ouverture dans une instance déjà lancée) — remplacer le délégué de `winit` risquerait de casser son cycle de vie de fenêtres, non vérifiable sans session graphique ; risque jugé disproportionné.
- [x] **#45 (UI) — Exposer l'export optimisé.** Nouveau `Session::export_optimized` (wrapper d'une ligne) + bouton "🗜 Optimiser…" (`NSSavePanel`).
- [ ] **#49 — Onglets multi-documents.** **Arbitré avec le développeur : reporté à un sprint dédié séparé**, vu l'ampleur du changement d'architecture par rapport au reste de ce sprint.
- [ ] **#50 (accessibilité) — VoiceOver + clavier 100 %.** Pas engagé dans cette passe.
- [x] **#50 (déchiffrement) — Ouverture de PDF chiffrés (mot de passe utilisateur vide).** **Arbitré avec le développeur : engagé, RC4 d'abord puis AES si le temps le permet** — nouveau module `pdf-core::crypt` (RC4 implémenté à la main, validé contre le vecteur de test RFC officiel ; AES-128/256 via `aes`/`cbc`, MD5/SHA-2 via `md-5`/`sha2`). En pratique les deux fixtures existants se sont révélés être AES (pas RC4) une fois inspectés en détail — `encrypted_rc4.pdf` = AES-128 (R4), `encrypted_aes256.pdf` = AES-256 (R6, hachage "renforcé") — donc c'est le chemin AES qui a fini par être validé de bout en bout sur le corpus réel, RC4 restant validé uniquement par le vecteur de test RFC (aucun fixture réel ne l'exerce). Câblé dans `Document::open`/`resolve`, déchiffre avant application des filtres. Les deux fixtures ajoutés aux tests `golden` (rendu comparé pixel par pixel, pas seulement le texte recomposé).

**Critère de sortie : atteint pour #1 (partiel)/#45/#50-déchiffrement.** `cargo test --workspace` vert (213 tests), `cargo clippy`/`cargo fmt` propres, `.app` généré et vérifié valide. #49 (onglets) et #50-accessibilité chiffrés en tickets séparés, comme prévu si le développeur choisissait de ne pas les engager immédiatement.

---

## Différé volontairement (Could / gros effort, faible valeur immédiate)

Ces lignes restent hors des 5 sprints ci-dessus par choix de séquencement (cohérent avec la logique déjà appliquée dans `sprint.md`, ex. 6c) :

- **#7** (historique de navigation retour arrière) — Could, confort mineur.
- **#12** (fond de page en mode sombre) — le chrome est déjà sombre, l'inversion du fond de page est cosmétique et peut heurter la fidélité de rendu (un PDF blanc doit-il vraiment s'afficher inversé ?) — à trancher avec le développeur avant d'investir dessus.
- **#18-19** (blend modes, gradients/shadings/patterns) — niveau 3 de la grille de conformité PDF (STATUS.md §2bis), motivé seulement par des PDF complexes rares en pratique.
- **#27-29, #31** (notes, formes, dessin main levée, signature) — nouveaux types d'annotation, chacun un mini-projet UI+moteur ; à ne prioriser qu'après avoir vidé les Sprints 18-22, qui ferment déjà l'essentiel des trous Must.
- **#42** (caviardage sécurisé) — nécessite une vraie suppression de contenu (pas juste masquer), donc une extension de 6c (édition chirurgicale du flux) — explicitement un projet de recherche séparé selon `sprint.md`.
- **#44** (flatten réel) — dépend d'abord d'avoir plusieurs types d'annotations/champs à aplatir (#27-31, #43 complet) pour être vraiment utile.
- **#47** (auto-save/Versions macOS) — Should, nécessite une intégration `NSDocument` plus profonde que le `NSSavePanel` actuel ; à évaluer une fois le multi-documents (#49) tranché, les deux sujets sont liés architecturalement.

---

## Points à trancher avec le développeur avant d'ouvrir ces sprints

1. **#11 vs #35** : le produit veut-il une rotation de *vue* éphémère (non sauvegardée) en plus de la rotation de *page* persistée ? Si non, retirer #11 de la grille d'audit future pour éviter un double comptage.
2. **#48 (impression)** : approche (a) `NSPrintOperation` natif vs (b) PDF temporaire ouvert dans Aperçu — impacte fortement l'effort du Sprint 21.
3. **#49 (onglets)** : **arbitré, puis engagé** — d'abord reporté à un sprint dédié séparé vu l'ampleur du changement d'architecture, puis le développeur a choisi de l'engager dans la foulée (Sprint 49, voir `sprint.md`). Fermé : `pdf-ui` refondu en `DocumentTab` (état par document) + `ViewerApp` (coordination globale, barre d'onglets, `⌘T`/`⌘W`). #47 (auto-save/Versions) reste en attente, non redécidé à ce stade.
4. **#50 (déchiffrement)** : **arbitré** — engagé (RC4 d'abord, puis AES si le temps le permet). Résultat : les deux fixtures existants se sont révélés être de l'AES en pratique (`encrypted_rc4.pdf` = AES-128 malgré son nom, `encrypted_aes256.pdf` = AES-256/R6), donc c'est le chemin AES qui a été validé de bout en bout sur le corpus réel ; RC4 est implémenté et validé uniquement contre le vecteur de test RFC officiel (aucun fixture réel du corpus ne l'exerce).

---

## Résumé exécutif

| Sprint | Thème | Lignes fermées (Must d'abord) | Effort dominant |
|---|---|---|---|
| 18 ✅ | Navigation & confort | #3, #22, #23, #25 | UI légère, moteur déjà prêt |
| 19 ✅ | Manipulation de pages → UI | #33, #34, #35, #36, #37, #38 | UI seule (moteur 100 % fait) |
| 20 ✅ (sauf #43) | Annotation & édition → UI | #26, #30, #32 (partiel), #40 | UI + une petite extension moteur (#43, reportée) |
| 21 ✅ (sauf vérif. #48) | Durcissement viewer | #9, #10, #20, #48 (à vérifier) | Moteur/systèm (thread de rendu, impression) |
| 22 ✅ (sauf a11y) | Intégration système | #1 (partiel), #45, #50-déchiffrement | Mixte (packaging, crypto) |
| 49 ✅ | Onglets multi-documents | #49 | Refonte d'architecture `pdf-ui` (`DocumentTab`/`ViewerApp`) |
| 50 ✅ | Remplissage formulaire au clic | #43 (partiel, `/Tx` seulement) | UI + petite extension moteur (localisation des widgets) |
| 51 ✅ | Poignées annotation (déplacer/redimensionner) | #32 (partiel, poignées seulement) | Moteur (rescale `/QuadPoints`) + UI (glissement, poignées de coin) |
| 52 ✅ | Cases à cocher AcroForm | #43 (partiel, `/Btn` en plus de `/Tx`) | Moteur (`/AS`+`/V`, pas de régénération d'apparence) + UI (clic direct) + nouveau fixture |

**Résultat réel après les Sprints 18-22 + 49-52 :** couverture Must 54 % → 81 % (voir `audit50quest.md`), zéro ligne Must totalement absente, **plus aucun chantier Must de fond ouvert**. #32 et #43, les deux seules lignes Must/Should encore à l'état "moteur prêt, UI absente" en fin de Sprint 22, sont maintenant partiellement fermées hors sprint dédié (Sprints 50-52) : #43 pour les champs texte (Sprint 50) et les cases à cocher (Sprint 52) — restent les boutons radio groupés et les listes (`/Ch`) ; #32 pour le déplacement/redimensionnement (Sprint 51) — reste la couleur/opacité. Il reste 9 lignes Must ◐, dont une seule par incertitude de vérification plutôt que par périmètre (#48, à tester manuellement) ; le reste par limite documentée délibérée. #49 (onglets), un temps reporté à un sprint dédié séparé par décision du développeur, a finalement été engagé et fermé juste après dans la même session — refonte de `pdf-ui` en `DocumentTab` (état par document) + `ViewerApp` (coordination globale), barre d'onglets, `⌘T`/`⌘W`. Côté Should, #50 (déchiffrement) est fermé pour le cas courant (mot de passe vide) ; l'accessibilité (#50, VoiceOver/clavier) reste à faire — seul vrai point ouvert restant, mais hors Must.
