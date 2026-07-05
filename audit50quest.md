# Audit — 50 fonctionnalités d'un viewer / éditeur PDF (PapyrusPDF)

**Date de l'audit :** 2026-07-05
**Méthode :** confrontation ligne à ligne de la grille avec le code réel (`pdf-core`, `pdf-render`, `pdf-render-gpu`, `pdf-text`, `pdf-edit`, `pdf-app`, `pdf-ui`, `pdf-cli`), `STATUS.md`, `sprint.md`, et vérifications directes (`grep`) sur les points non documentés explicitement (impression, association Finder, onglets, accessibilité, go-to-page, tri-clic, export `.txt`).

Légende : ☐ Absent · ◐ Partiel · ☑ Présent — Prio : M Must · S Should · C Could — Moteur : ● cœur moteur · ○ UI/app.

---

## A · Ouverture & navigation

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 1 | Ouvrir un PDF (bouton, DnD, double-clic Finder) | M | ○ | ◐ | Bouton + dialogue natif `rfd`, glisser-déposer géré par `egui`/`winit`. **Amélioré Sprint 22** : `CFBundleDocumentTypes` ajouté (`pdf-ui/document_types.plist.xml` via `osx_info_plist_exts`, vérifié dans l'`Info.plist` généré) — l'app apparaît maintenant dans "Ouvrir avec…" du Finder pour les PDF. **Toujours pas de double-clic → ouverture automatique dans une instance déjà lancée** : nécessiterait `NSApplicationDelegate::application:openURLs:`, délibérément non fait (remplacer le délégué `winit` risquerait de casser le cycle de vie des fenêtres, non vérifiable sans session graphique). |
| 2 | Navigation précédente/suivante + n° de page | M | ○ | ☑ | `Session::next_page`/`prev_page`/`goto_page` bornés (`pdf-app`), boutons dans `pdf-ui/src/main.rs`. |
| 3 | Aller directement à une page (⌘G) | M | ○ | ☑ | **Fermé Sprint 18.** Champ de saisie dans la barre d'outils + raccourci `⌘G` (`pdf-ui/src/main.rs::goto_page_from_input`), réutilise `Session::goto_page` déjà bornée. |
| 4 | Modes défilement : continu / page unique / double page | M | ○ | ◐ | Continu (toggle "📜 Continu", `egui::ScrollArea::show_rows` virtualisée) et page unique existent. **Pas de mode double page (face-à-face).** |
| 5 | Miniatures (sidebar) avec saut de page | M | ● | ☑ | `egui::SidePanel`, une miniature par page (`THUMBNAIL_SCALE`), clic → `goto_page`. |
| 6 | Signets / plan (outline) cliquables | S | ● | ☑ | `pdf-core::outline` (`/Outlines`) + `Session::outline()` + panneau arborescent `pdf-ui`. Limite : destinations nommées (`/Names/Dests`) non résolues. |
| 7 | Historique de position + retour arrière | C | ○ | ☐ | Aucune pile d'historique de navigation trouvée. |

## B · Zoom & affichage

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 8 | Zoom avant/arrière + niveau % (⌘+/⌘−) | M | ● | ☑ | Boutons ＋/－/réinitialiser, re-rasterisation via `pdf_render::render_page_scaled`. |
| 9 | Zoom au pincement trackpad (centré curseur) | M | ● | ☑ | **Fermé Sprint 21.** Le décalage de défilement est recalculé à chaque pincement à partir du dernier connu (`last_scroll_offset`/`last_scroll_viewport`) pour garder le point sous le curseur fixe à l'écran. |
| 10 | Ajuster à la largeur / à la page / taille réelle | M | ● | ☑ | **Fermé Sprint 21.** "Ajuster à la largeur" (déjà fait) + "↕ Ajuster à la page" (nouveau, `fit_to_page`) + "Réinitialiser" explicitement documenté comme "Taille réelle (100 %)". |
| 11 | Rotation d'affichage de la page (90°, non persistée) | S | ○ | ☐ | Aucune occurrence de rotation dans `pdf-ui/src/main.rs`. Seule la rotation *persistée* de page existe côté moteur (`pdf-edit::rotate_page`, voir #35), sans UI. |
| 12 | Mode sombre (chrome + fond de page) | M | ○ | ☑ | Bascule réelle `NSApplication.appearance` synchronisée avec `ctx.set_visuals` (`native_menu.rs`, Sprint 11-12). |
| 13 | Plein écran / Split View macOS | M | ○ | ☑ | Plein écran natif `⌃⌘F` (`toggleFullScreen:` AppKit standard) ; Split View est une propriété native de toute `NSWindow` standard, hérité gratuitement. |

## C · Rendu (couche moteur)

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 14 | Polices intégrées (Type1/TrueType/CFF/CID) | M | ● | ◐ | TrueType (`/FontFile2`), CFF/Type1C (`/FontFile3`), composites `/Type0` `CIDFontType0`/`CIDFontType2` — tous testés sur fixtures réels (`pdf-core/src/font.rs`). **Type1 historique (`/FontFile`, pré-CFF) non fait** : aucun contour (`eexec`/charstrings Type1 non décodés). |
| 15 | Substitution des polices manquantes | M | ● | ☑ | Lecture directe `/System/Library/Fonts` (Helvetica/Times/Courier/Symbol/ZapfDingbats + alias Arial, gras/italique). Limite : chemins macOS codés en dur, pas via Core Text. |
| 16 | Rendu des images (JPEG/Flate, CMJN, indexé) | M | ● | ◐ | JPEG RGB/CMYK (`zune-jpeg`) + Flate + `/SMask` alpha fonctionnels. **Indexed/Separation/Lab, CCITT/JBIG2/JPX non supportés** (dégradation propre, pas de crash). |
| 17 | Rendu vectoriel (chemins, remplissage, tracé, AA) | M | ● | ☑ | `tiny-skia` : fill/stroke/fill+stroke, nonzero/even-odd, Bézier, anti-aliasing natif ; comparé pixel par pixel (`pdf-render/tests/golden.rs`, 23 fixtures). |
| 18 | Transparence & blend modes | S | ● | ◐ | Alpha réel via `/SMask` (mélange vrai, testé pixel par pixel). **Blend modes non gérés** ; `/ca` (opacité ExtGState) non lu par l'interpréteur (une annotation surlignée est rendue en couleur pleine, pas semi-transparente). |
| 19 | Dégradés / shadings / patterns | S | ● | ☐ | Explicitement ignorés par l'interpréteur de contenu (`pdf-core/src/interp.rs`, voir grille de conformité §2bis de STATUS.md, niveau 3). |
| 20 | Rendu progressif / par tuiles hors thread UI | M | ● | ☑ | **Fermé Sprint 21** (au sens "hors thread UI" ; toujours pas de tuiles au sens dalles d'une page). `pdf-ui/src/render_worker.rs` : thread dédié qui rastérise miniatures et pages du défilement continu (`Session::current_bytes` → reparsing indépendant, pas de partage `Rc`/`RefCell` entre threads). La page unique reste synchrone (délibéré, une seule page, risque minime). |

## D · Texte : recherche, sélection, extraction

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 21 | Recherche plein texte live (⌘F, surlignage, compteur, ⌘G) | M | ● | ◐ | Recherche plein document + surlignage jaune + saut cyclique entre occurrences (`jump_to_match`, `main.rs`). **Pas de compteur "X/Y occurrences" visible confirmé**, pas de reconstruction par blocs/colonnes (largeur de glyphe approximée). |
| 22 | Recherche insensible casse/accents | S | ● | ☑ | **Fermé Sprint 18.** `pdf_text::fold_char_for_search`/`normalize_for_search` (décomposition NFD, ne garde que le caractère de base) — "Étudié" retrouve "etudie". Repliement caractère par caractère (limite inchangée pour les scripts non latins à casse multi-caractères). |
| 23 | Sélection de texte (glisser, double-clic mot, triple-clic ligne) | M | ● | ◐ | Glisser fonctionnel + **double-clic (mot)/triple-clic (ligne) fermés Sprint 18** (`PageText::word_range_at`/`line_range_at`, câblés dans `handle_text_selection`). **Reste :** sélection toujours limitée au mode page unique (pas en défilement continu). |
| 24 | Copier le texte (⌘C) dans le bon ordre | M | ● | ☑ | `Event::Copy` + bouton "📋 Copier", texte extrait dans l'ordre d'émission du flux de contenu. |
| 25 | Extraction / export du texte (fichier .txt) | S | ● | ☑ | **Fermé Sprint 18.** `Session::extract_all_text` (tout le document, un saut de page `\x0c` par page) + bouton "📄 Exporter le texte…" (`NSSavePanel` via `rfd`). |

## E · Annotations & commentaires

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 26 | Surligner / souligner / barrer | M | ● | ☑ | **Fermé Sprint 20.** `/Highlight` (bouton "🖍 Surligner") + `/Underline`/`/StrikeOut` (nouveaux, `add_underline_annotation`/`add_strikeout_annotation`, boutons "Souligner"/"Barrer") — les trois réutilisent `selection_bbox`. |
| 27 | Notes ancrées (post-it) + panneau commentaires | S | ○ | ☐ | Aucun support `/Text` (note), aucun panneau de commentaires. |
| 28 | Formes (rectangle, ellipse, ligne, flèche) | S | ○ | ☐ | Non implémenté (voir sprint.md Sprint 13-14 : "notes, formes... non faits"). |
| 29 | Dessin à main levée | C | ○ | ☐ | Non implémenté. |
| 30 | Zone de texte libre (FreeText) | S | ○ | ☑ | **Fermé Sprint 20.** Bouton bascule "📝 Ajouter texte" → clic sur la page → boîte de dialogue modale → `add_free_text_on_current_page`. Boîte de taille fixe (`NEW_TEXT_BOX_SIZE`), pas encore redimensionnable par l'utilisateur. |
| 31 | Signature (tracer/importer, poser, redimensionner) | S | ○ | ☐ | Non implémenté, ni moteur ni UI. |
| 32 | Éditer/déplacer/supprimer une annotation (poignées, couleur, opacité) | M | ○ | ◐ | **Amélioré Sprint 20** (reste ◐, pas ☑) : `Session::annotations_on_current_page` liste les annotations de la page, `pdf-ui` dessine un contour cliquable par annotation et propose "🗑 Supprimer l'annotation" au clic. **Toujours pas de poignées de déplacement/redimensionnement ni de réglage couleur/opacité après création** — délibérément hors scope de cette passe. |

## F · Manipulation de pages

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 33 | Réorganiser les pages par glisser-déposer (miniatures) | M | ● | ☑ | **Fermé Sprint 19.** `egui::Ui::dnd_drag_source`/`dnd_drop_zone` dans `show_thumbnail_panel` → `Session::move_page`. |
| 34 | Supprimer des pages | M | ● | ☑ | **Fermé Sprint 19.** Bouton 🗑 par miniature → `Session::delete_page_at`. |
| 35 | Pivoter des pages (rotation persistée) | S | ● | ☑ | **Fermé Sprint 19.** Bouton ↻ par miniature (+90°) → `Session::rotate_page_at`. |
| 36 | Insérer des pages (vierge, image, autre PDF) | S | ● | ☑ | **Fermé Sprint 19.** Boutons "＋ Page"/"🖼 Image…" (`NSOpenPanel`) → `insert_blank_page_at`/`insert_image_page_at`. Limite inchangée : JPEG seulement (héritée de `pdf-edit`). |
| 37 | Fusionner plusieurs PDF | S | ● | ☑ | **Fermé Sprint 19.** Bouton "📎 PDF…" (`NSOpenPanel`) → `Session::merge_document_from_path`. Limite inchangée : un seul niveau d'`/AcroForm` entre documents fusionnés. |
| 38 | Découper / extraire des pages + sélection multiple | S | ● | ☑ | **Fermé Sprint 19.** Case à cocher par miniature (`thumbnail_selection`) + bouton "✂ Extraire (N)…" (`NSSavePanel`) → `Session::extract_pages_to_file`. |

*(Le groupe F est désormais entièrement câblé à l'UI depuis le Sprint 19 — voir [sprint.md](./sprint.md#sprint-19--câbler-la-manipulation-de-pages-à-pdf-ui). Avant ce sprint, le moteur `pdf-edit` couvrait déjà toutes ces opérations, testées bout en bout, mais aucune n'était déclenchable depuis l'interface graphique — seulement via `pdf-cli` ou l'API Rust directe.)*

## G · Édition de contenu

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 39 | Ajouter du texte natif (IME : accents, dictée) | S | ● | ◐ | `add_free_text_annotation`/`replace_text_with_overlay` fonctionnels (police Helvetica allouée à la volée), mais texte sur une seule ligne, positionnement manuel (`Td`), pas de retour à la ligne automatique. Aucune saisie interactive dans `pdf-ui` (donc pas de test IME réel). |
| 40 | Éditer un texte existant (superposition) | S | ● | ☑ | **Fermé Sprint 20** (au sens de cette ligne, "approche par superposition") : bouton "✏ Remplacer…" (actif sur une sélection) → dialogue préremplie → `replace_text_on_current_page` (6b : masque + redessine, flux original toujours extractible). **6c (édition chirurgicale du flux, subsets de polices)** reste volontairement non engagé, traité comme projet de recherche séparé — hors du périmètre décrit par cette ligne. |
| 41 | Insérer / remplacer / redimensionner des images | S | ● | ◐ | `insert_image_page` (JPEG tel quel, dimensionné à la taille réelle). **Pas de remplacement d'image existante, pas de redimensionnement manuel, pas de PNG**, aucune UI. |
| 42 | Caviardage sécurisé (suppression réelle du contenu) | S | ● | ☐ | Non implémenté. Le mécanisme le plus proche (6b, superposition) **cache visuellement mais laisse le texte extractible** — l'inverse de ce qu'exige un caviardage sécurisé (STATUS.md le documente explicitement comme une limite). |

## H · Formulaires & signature électronique

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 43 | Remplissage AcroForm (texte, cases, listes) | S | ● | ◐ | Champs texte seulement (`set_form_field_value`, testé bout en bout sur `acroform_textfield.pdf`). **Pas de cases à cocher/boutons radio/listes**, aucune UI (pas de clic sur un champ dans `pdf-ui`). **Explicitement non traité au Sprint 20** (nécessite d'abord de localiser/exposer les widgets `/Annots` de type `/Widget` d'une page — un vrai sous-morceau de travail, pas juste un câblage). |
| 44 | Aplatir les champs/annotations à l'export | S | ● | ☐ | `export_optimized` est un garbage-collector par reconstruction (objets atteignables), **pas un vrai flatten** (fusion de l'apparence dans le flux de contenu + suppression de l'interactivité). Non implémenté au sens strict. |

## I · Fichiers, sauvegarde & impression

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 45 | Enregistrer (incrémental) / Enregistrer sous / Export optimisé | M | ● | ☑ | **Fermé Sprint 22.** "Enregistrer" (`⌘S`), "Exporter une copie…" (`⇧⌘S`) et désormais "🗜 Optimiser…" (nouveau `Session::export_optimized` + `NSSavePanel`) tous câblés dans l'UI. |
| 46 | Annuler / Rétablir (⌘Z/⌘⇧Z) par action | M | ● | ◐ | Fonctionnel et câblé (boutons + raccourcis natifs, activation conditionnelle). **Limite documentée** : les objets nouvellement créés par une opération (annotation, apparence, police) ne sont pas physiquement supprimés par `undo` — ils deviennent juste non référencés (orphelins), pas retirés tant qu'un `export_optimized` n'est pas lancé. |
| 47 | Auto-save + Versions macOS + restauration d'état | S | ○ | ☐ | Aucune intégration `NSDocument`/Versions, aucune restauration de session à la réouverture (pas de multi-documents, voir #49). |
| 48 | Imprimer (dialogue système, aperçu, sélection de pages) | M | ● | ◐ | **Ajouté Sprint 21**, mais non vérifié interactivement : "Imprimer…" (`⌘P`) écrit le document courant vers un fichier temporaire puis délègue à Aperçu via AppleScript (`print ... with properties {print dialog:true}`) plutôt qu'un `NSPrintOperation` maison. Donne l'aperçu et la sélection de pages du système **si le script AppleScript se comporte comme attendu** — l'environnement de développement n'a pas d'accès GUI pour le confirmer, à tester manuellement. |

## J · Intégration système, accessibilité & sécurité

| N° | Fonctionnalité | Prio | Moteur | Statut | Preuve |
|---|---|---|---|---|---|
| 49 | Barre de menus + raccourcis natifs + onglets multi-documents | M | ○ | ☑ | **Fermé Sprint 49.** Vraie `NSMenu` (`objc2`/`objc2-app-kit`), raccourcis Fichier/Édition/Affichage/Fenêtre fonctionnels (`native_menu.rs`). Onglets multi-documents : refonte `pdf-ui` (`DocumentTab` porte tout l'état par document, `ViewerApp` porte `Vec<DocumentTab>` + ce qui est global), barre d'onglets, `⌘T`/`⌘W` (nouvel onglet/fermer l'onglet). Limites mineures : pas de raccourci clavier pour changer d'onglet, pas de réordonnancement par glisser-déposer. |
| 50 | VoiceOver + navigation 100 % clavier + PDF chiffrés (mot de passe) | S | ● | ◐ | **Déchiffrement fermé au Sprint 22** : nouveau `pdf-core::crypt` (RC4/AES-128 révisions 2-4, AES-256 révisions 5/6 avec hachage "renforcé" R6) — mot de passe utilisateur **vide** géré (cas réel le plus courant), vérifié de bout en bout sur les deux fixtures existants (rendu + texte recomposé, ajoutés aux tests `golden`). **Reste absent :** vrai mot de passe utilisateur (pas d'UI de saisie), et toute accessibilité (`NSAccessibility`/VoiceOver, aucune trace dans le code). |

---

## Score de couverture

**Couverture Must = (Must ☑ + ½ Must ◐) / total Must**

24 lignes sont classées **Must** dans la grille : 1, 2, 3, 4, 8, 9, 10, 12, 14, 15, 16, 17, 20, 21, 23, 24, 26, 32, 33, 34, 45, 46, 48, 49.

| Statut | N° | Compte |
|---|---|---|
| ☑ | 2, 3, 8, 9, 10, 12, 15, 17, 20, 24, 26, 33, 34, 45, 49 | 15 |
| ◐ | 1, 4, 14, 16, 21, 23, 32, 46, 48 | 9 |
| ☐ | — | 0 |

**Couverture Must = (15 + 0,5 × 9) / 24 = 19,5 / 24 ≈ 81 %** (54 % initial → 58 % [Sprint 18] → 62 % [Sprint 19] → 67 % [Sprint 20] → 77 % [Sprint 21] → 79 % [Sprint 22] → 81 % après [Sprint 49](./sprint.md#sprint-49--onglets-multi-documents) : #49 fermé — **toujours zéro ligne Must totalement absente**). Hors calcul Must (Should) : #50 (déchiffrement) fermé au Sprint 22 pour un mot de passe utilisateur vide — voir sa ligne.

Le socle Must est maintenant très solide : plus des quatre cinquièmes pleinement acquis ou à moitié acquis, zéro ligne Must reste à l'état "rien du tout", et le dernier vrai chantier de fond identifié (#49, onglets multi-documents) est refermé. Les 9 lignes ◐ restantes sont pour la plupart des limites connues et documentées (repli approximatif, casse multi-caractères, mode continu), plus #48 (à vérifier interactivement, voir ci-dessous).

### Dernier point Must à vérifier avant de le considérer clos : #48 (Imprimer)

Implémenté au Sprint 21 (délégation à Aperçu via AppleScript) mais **non vérifié interactivement** — l'environnement de développement qui a produit ce code n'a pas d'accès à une session graphique macOS pour confirmer que la boîte de dialogue d'impression s'ouvre réellement comme attendu. C'est la seule ligne Must dont le statut ◐ reflète une incertitude de vérification plutôt qu'une limite de périmètre délibérée — à tester manuellement en priorité.

### Les lignes qui tirent le plus l'UX vers le bas (fortement moteur, actuellement ◐)

- **#21 (recherche)** : fonctionnelle mais sans compteur d'occurrences ni reconstruction par blocs — utilisable mais perçue comme "brute".
- **#23 (sélection)** : le glisser et le double/triple-clic marchent (Sprint 18) ; reste la sélection en mode défilement continu, absente.
- **#14/#16 (fidélité de rendu)** : très solide (niveau 1 de la grille de conformité PDF entièrement acquis, niveau 2 bien avancé), le vrai manque restant est Type1 historique et les espaces colorimétriques avancés — un manque de fond de dossier, pas un manque quotidien.
- **#32 (annotations)** : sélection/suppression fermées au Sprint 20, mais toujours pas de poignées de déplacement/redimensionnement ni de réglage couleur/opacité après création.

### Le piège connu : ligne 40

Confirmé dans le code : 6a/6b (ajout de texte, remplacement par superposition) sont livrés et testés bout en bout, **et désormais câblés à l'UI depuis le Sprint 20** (boutons "📝 Ajouter texte"/"✏ Remplacer…"). 6c (édition chirurgicale du flux de contenu existant + gestion des subsets de polices) reste **volontairement non engagé** et traité comme un projet de recherche séparé (`sprint.md`, Sprint 17+). C'est la bonne décision de séquencement — à ne pas rouvrir avant d'avoir vidé le reste du backlog Must/Should.

### Groupe F (page manipulation) et une bonne partie du groupe E : refermés depuis les Sprints 19-20

Les 6 fonctionnalités de manipulation de pages (33-38) et 3 des 7 lignes d'annotation (26, 30, et la moitié de 32) sont passées de "moteur fait, zéro UI" à câblées dans `pdf-ui` — voir `sprint.md` Sprints 19 et 20. Ce qui reste dans cet état ("moteur prêt, UI absente ou partielle") : #32 (poignées de déplacement/redimensionnement/opacité) et #43 (formulaire au clic, explicitement reporté au Sprint 20 faute d'une fonction de localisation des widgets déjà prête).
