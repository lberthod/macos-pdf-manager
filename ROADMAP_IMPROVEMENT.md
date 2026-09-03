# Feuille de route — combler les écarts identifiés

Ce document part du comparatif critique établi le 29 août 2026 face à Adobe
Acrobat Pro, Foxit PDF Editor, PDF Expert et Preview (macOS) — publié comme
artefact HTML, pas versionné dans ce dépôt — et détaille, pour chaque critère
resté à l'état **Partiel** ou **Absent**, les étapes concrètes pour le faire
passer à **Complet**. Classé par priorité (impact utilisateur × risque de ne
pas le faire), pas par facilité.

Pour le détail des correctifs de sécurité/qualité déjà appliqués (distinct de
cette feuille de route, qui porte sur des fonctionnalités manquantes), voir
[AUDIT_FIXES.md](./AUDIT_FIXES.md).

Légende d'effort : **S** (quelques jours) · **M** (1-2 semaines) · **L**
(3-6 semaines) · **XL** (chantier de fond, plusieurs mois).

---

## Priorité 1 — Bloquants pour un usage réel

### 1. OCR — absent → complet

**Pourquoi en premier :** une part massive des PDF réels sont des scans sans
couche texte. Sans OCR, PapyrusPDF ne fait qu'afficher une image : pas de
recherche, pas de sélection, pas d'extraction. C'est le seul écart qui rend
la moitié des cas d'usage réels totalement hors de portée, quel que soit le
reste.

**Étapes :**
1. Choisir le moteur : `tesseract-rs`/`leptess` (bindings Tesseract, mature,
   licence permissive) plutôt que réimplémenter un moteur de reconnaissance —
   hors de portée d'un projet de cette taille. Alternative pure-Rust encore
   jeune : `ocrs` (projet Rust natif, pas de dépendance C++) — à évaluer sur
   la qualité de reconnaissance avant de trancher, mais évite d'introduire
   une dépendance système (Tesseract doit être installé sur la machine de
   build/l'utilisateur, ce qui complique le packaging `.app`/`.dmg`).
2. Nouveau crate `pdf-ocr` : prend une image décodée (`pdf-core::filters`
   expose déjà les pixels bruts des images `/DCTDecode`/`/FlateDecode`) et
   produit un texte positionné, dans le même format que
   `pdf_text::PageText` (glyphe + rect) pour réutiliser tout le pipeline de
   recherche/sélection existant sans le dupliquer.
3. Dans `pdf-edit` : une opération `add_invisible_text_layer` qui injecte le
   texte reconnu en couche invisible (`Tr 3`, mode de rendu "invisible") au-
   dessus de l'image scannée — pattern standard ("PDF searchable"), déjà
   documenté dans la doc `interp.rs` pour l'opérateur `Tr`.
4. UI (`pdf-ui`) : bouton "Rendre ce document recherchable…" (par page ou
   document entier), avec barre de progression (l'OCR est lent, doit tourner
   sur `render_worker.rs`, pas sur le thread UI).
5. Fixture de test : au moins un PDF scanné réel avec un texte connu à
   reconnaître, assertion sur le texte extrait après OCR (tolérance sur les
   erreurs de reconnaissance, pas une égalité stricte).

**Effort : XL.** Le plus gros chantier de cette liste, mais aussi celui qui
débloque le plus d'usages réels.

---

### 2. Caviardage sécurisé — absent → complet (métadonnées/XMP en suspens)

**Pourquoi en priorité 2 :** ce n'est pas qu'une fonctionnalité manquante,
c'est un piège actif. Le mécanisme le plus proche (`replace_text_with_overlay`)
est déjà documenté comme *non sécurisé pour cet usage* — le texte "caché"
reste extractible. Ne pas le corriger, c'est laisser un angle mort connu.

**Étapes :**
1. Dans `pdf-edit`, nouvelle opération `redact_region(page, rect)` distincte
   de `replace_text_with_overlay` (qui doit continuer à exister pour son cas
   d'usage actuel — l'édition visuelle non sécurisée est un besoin légitime
   documenté, le nouveau caviardage n'a pas vocation à le remplacer).
2. `redact_region` doit réellement retirer le contenu sous la zone, pas
   seulement le recouvrir :
   - Reparser le flux de contenu de la page (`pdf-core::content`) et
     découper/retirer les opérateurs de dessin (`Tj`/`TJ`, chemins de
     remplissage, images) dont le bounding box intersecte la zone à
     caviarder — repose sur `pdf-core::interp` qui calcule déjà ces
     bounding boxes pour la `DisplayList`, donc la géométrie existe déjà.
   - Pour une image partiellement couverte, ce n'est pas suffisant de
     retirer l'opérateur `Do` en entier si l'image déborde la zone : il faut
     soit interdire le caviardage partiel d'image (message d'erreur clair,
     l'utilisateur doit caviarder l'image entière), soit implémenter un
     recadrage de l'image elle-même (XL en soi, à ne pas faire dans un
     premier temps).
   - Dessiner un rectangle plein (comme aujourd'hui) par-dessus, pour le
     rendu visuel.
3. Nettoyage des métadonnées : `Document::trailer`, `/Info`, XMP
   (`/Metadata`) peuvent contenir le texte caviardé (aperçus, historique de
   révision Acrobat, etc.) — étape de nettoyage séparée et documentée comme
   telle (ne pas prétendre à un caviardage complet tant que cette étape
   n'existe pas).
4. `export_optimized` (déjà existant, garbage-collect par reconstruction)
   doit être appelé automatiquement après un caviardage, pour s'assurer que
   les anciens objets non référencés (l'ancien contenu retiré) ne traînent
   pas dans le fichier sous forme d'objets orphelins encore lisibles par un
   outil de récupération de PDF.
5. Test de non-régression explicite : après caviardage + sauvegarde +
   réouverture, `pdf-text::extract_text` sur la zone caviardée ne doit
   renvoyer **aucun** caractère du texte original — le test qui, à lui
   seul, distingue un vrai caviardage d'un cache visuel.
6. UI : renommer clairement le bouton actuel ("Remplacer…") pour ne jamais
   laisser croire qu'il caviarde, et ajouter un nouveau bouton "Caviarder…"
   distinct, avec une confirmation explicite avant application (action
   destructive et irréversible par nature).

**Effort : L.** Le retrait d'opérateurs texte est relativement direct ; le
cas des images partiellement couvertes et le nettoyage de métadonnées sont
ce qui pousse l'estimation vers L vs M.

**Statut (2026-09-03) :** étapes 1, 2 (variante "tout ou rien" pour
images/formulaires plutôt que le recadrage XL), 4 et 5 faites, étape 6
faite côté application (pas la barre native macOS) :
- `pdf_edit::EditSession::redact_region` (`pdf-edit/src/redact.rs`) retire
  réellement texte/chemins/images dont la zone croise le rectangle donné.
- `EditSession::save_redacted` (remplace `save_as` après une redaction)
  réécrit le fichier en entier via `export_optimized` plutôt qu'une simple
  sauvegarde incrémentale — sans ça, l'ancien flux de contenu caviardé
  resterait physiquement présent dans le fichier, juste superseded dans la
  xref. `pdf_app::Session` bascule automatiquement dessus (`has_redaction`)
  dès qu'une redaction a eu lieu dans la session.
- Exposé via `pdf-cli redact` et, dans `pdf-ui`, un bouton "Caviarder…"
  distinct de "Remplacer…" avec confirmation explicite avant application.
- Testé bout en bout, y compris au niveau des octets bruts du fichier
  sauvegardé (pas seulement en reparsant normalement) : voir
  `pdf-edit/src/lib.rs::redact_region_actually_removes_the_underlying_text`.

Reste : le nettoyage des métadonnées/XMP (`/Info`, `/Metadata`) n'est pas
fait — documenté comme limite connue, pas encore un chantier engagé — et
les images en ligne (`BI`/`ID`/`EI`) sont volontairement rejetées avec une
erreur explicite plutôt que prises en charge.

---

### 3. Accessibilité du contenu de page (VoiceOver) — absent → complet

**Pourquoi en priorité 3 :** le chrome de l'app est déjà accessible
gratuitement (AccessKit/egui) — c'est *le contenu du document* qui est
invisible à un lecteur d'écran, rendu comme une texture opaque. Un
utilisateur de VoiceOver ne peut tout simplement pas lire un PDF dans l'app
aujourd'hui.

**Étapes :**
1. `pdf-core::interp` produit déjà une `DisplayList` avec chaque glyphe
   positionné et son caractère Unicode résolu (`DisplayItem::Glyph`) — la
   donnée existe, il manque le pont vers l'arbre d'accessibilité.
2. `egui`/`accesskit` permet d'exposer un arbre de nœuds accessibles
   personnalisé (`accesskit::Node`) au-delà des seuls widgets natifs —
   construire, à partir de `pdf_text::PageText` (déjà utilisé pour la
   sélection/recherche), un nœud de texte accessible par bloc de ligne, avec
   son rectangle de position (pour que VoiceOver puisse annoncer/naviguer
   par ligne comme sur une vraie page).
3. Reconstruire cet arbre à chaque changement de page/zoom (coûteux si fait
   naïvement à chaque frame — le mettre en cache par `(page, échelle)`,
   même stratégie que `Session::render_cache`/`text_cache` déjà en place
   dans `pdf-app`).
4. Test : pas d'automatisation simple possible pour VoiceOver lui-même
   (nécessite une session graphique + VoiceOver actif, indisponible en CI
   headless) — mais l'arbre `accesskit::Node` généré peut être testé
   unitairement (structure, textes, positions) sans lecteur d'écran réel.
5. Documenter la limite restante : les annotations `/FreeText` et les
   champs de formulaire ont aussi besoin d'un nœud accessible dédié, à
   traiter dans une deuxième passe une fois le texte de page couvert.

**Effort : L.** La donnée existe déjà (gros avantage par rapport à repartir
de zéro), le travail est surtout dans le pont vers `accesskit` et la gestion
du cache/invalidation.

---

## Priorité 2 — Complétude du moteur

### 4. Codecs image : CCITT, JBIG2, JPX — absent → complet

**Pourquoi ça compte :** CCITT et JBIG2 sont les codecs standards des PDF
scannés en noir et blanc (fax, photocopieurs, scanners de bureau) — un
sous-ensemble énorme et différent des JPEG couleur déjà supportés. JPX
(JPEG2000) est plus rare mais apparaît dans des flux médicaux/satellite.

**Étapes :**
1. **CCITT Group 3/4** : le plus simple des trois — algorithme de
   compression fax bien documenté (ITU-T T.4/T.6), implémentable à la main
   dans `pdf-core::filters` sur le modèle des prédicteurs PNG/TIFF déjà
   écrits maison. Bon premier pas, effort M.
2. **JBIG2** : nettement plus complexe (segmentation par régions,
   dictionnaires de symboles partagés entre pages) — évaluer une crate Rust
   existante (`jbig2dec`-bindings ou implémentation pure Rust si elle existe
   et est maintenue) plutôt que réimplémenter le standard complet à la main.
   Effort L à XL selon la voie choisie.
3. **JPX/JPEG2000** : le moins prioritaire des trois (le plus rare en
   pratique) — `jpeg2000`/bindings OpenJPEG si une crate Rust fiable existe,
   sinon reporter.
4. Chaque codec ajouté doit avoir son fixture réel dans
   `pdf-core/tests/fixtures/` (le corpus actuel documente déjà l'absence de
   ces trois codecs sur `scanned_page_like.pdf`) et un test de non-régression
   pixel (`pdf-render/tests/golden.rs`).

**Effort : M (CCITT) + L/XL (JBIG2) + M/L (JPX)**, traitable comme trois
sous-chantiers indépendants dans cet ordre.

### 5. Type1 historique (`/FontFile`) — absent → complet

**Étapes :**
1. Décoder le chiffrement `eexec` (algorithme trivial, déjà documenté dans
   la spec Type1 d'Adobe — un XOR à clé fixe, pas une vraie cryptographie).
2. Interpréter les charstrings Type1 (jeu d'opcodes différent de
   Type2/CFF — `hstem`/`vstem`/`hsbw`/`seac` etc.) : pas couvert par
   `ttf-parser` (qui ne gère que TrueType/CFF/OpenType), donc à écrire à la
   main dans `pdf-core::font`, sur le modèle du décodeur CFF déjà en place.
3. Fixture : au moins un PDF avec une police Type1 intégrée réelle (format
   de moins en moins rencontré, peut nécessiter un outil ancien type
   FontForge pour en générer un pour les tests).

**Effort : M.** Contenu, pas juste un test — un vrai morceau de moteur à
écrire, mais scope bien délimité (dernier trou de la grille de conformité
niveau 2, voir `docs/EXPLICATION.md`).

### 6. CMaps CJK prédéfinis/embarqués au-delà d'`Identity-H` — partiel → complet

**Étapes :**
1. Lire les CMaps prédéfinis les plus courants (`UniGB-UCS2-H`,
   `UniCNS-UCS2-H`, `UniJIS-UCS2-H`, `UniKS-UCS2-H`) — soit embarqués en
   ressource statique (fichiers CMap standards, taille raisonnable), soit
   un sous-ensemble couvrant les plages Unicode CJK les plus fréquentes.
2. Gérer les CMaps embarqués comme flux (`/Encoding` référence un flux
   plutôt qu'un nom) — parser le format CMap générique (`begincidrange`
   etc., proche de ce que `parse_to_unicode_cmap_wide` fait déjà pour
   `/ToUnicode`, réutilisable en grande partie).
3. Fixture : un PDF CJK réel utilisant un CMap prédéfini plutôt
   qu'`Identity-H` (à construire avec un outil comme `pikepdf` en spécifiant
   explicitement l'encodage).

**Effort : M.**

---

## Priorité 3 — Édition et formulaires

### 7. Annotations : formes, encre, notes, tampons — partiel → complet

**Étapes :**
1. `pdf-edit::EditSession` a déjà `add_highlight_annotation`/
   `add_free_text_annotation` comme modèle — ajouter, sur le même schéma
   (génération d'apparence `/AP /N` + insertion dans `/Annots`) :
   - `/Square`/`/Circle` (formes)
   - `/Ink` (dessin à main levée — nécessite de capturer une liste de
     points depuis `pdf-ui`, pas juste un rectangle de sélection)
   - `/Text` (note ancrée façon post-it, icône + popup de contenu)
   - `/Stamp` (tampon prédéfini ou image)
2. `pdf-ui` : un mode "dessiner" (main levée) qui capture les positions de
   la souris pendant le glisser, plutôt que juste début/fin comme
   aujourd'hui pour la sélection de texte — changement d'interaction, pas
   seulement de rendu.
3. Panneau de commentaires (liste des notes/annotations avec réponses) —
   nouveau composant `pdf-ui`, aucun équivalent existant à réutiliser.

**Effort : L**, décomposable annotation par annotation (formes = M, encre =
M, notes = S, tampons = S, panneau de commentaires = M).

### 8. Formulaires : JavaScript, XFA, création — partiel → complet

**Étapes :**
1. **JavaScript de formulaire** (`/AA`, `/JS`, calculs de champs) :
   nécessite un interpréteur JS embarqué (`boa` ou `rquickjs`, les deux
   existent en Rust pur/bindings) plus l'implémentation de l'API PDF
   `this.getField(...)`/`event.value` exposée aux scripts — chantier
   substantiel, à ne pas sous-estimer (c'est essentiellement écrire un
   mini-environnement d'exécution de formulaire).
2. **XFA** : format legacy, en fin de vie même chez Adobe (retiré de la
   spec ISO 32000-2) — probablement à ne **pas** faire, le rapport
   effort/valeur est mauvais pour un format que le secteur abandonne.
3. **Création de formulaire** (ajouter un nouveau champ AcroForm à un PDF
   qui n'en a pas) : plus accessible que le JS — `pdf-edit` a déjà toute la
   mécanique d'écriture d'objets/annotations, "ajouter un champ" est du même
   ordre que "ajouter une annotation `/FreeText`" déjà en place.

**Effort : XL (JavaScript) — probablement à dépriorister davantage, M pour
la création de champs (bien plus rentable en premier).**

---

## Priorité 4 — Plateforme et maturité produit

### 9. Multiplateforme (Windows/Linux) — absent → complet

Le code est déjà gaté proprement (`#[cfg(target_os = "macos")]` sur
`objc2`/`native_menu`, voir le commit récent) — ce qui manque est le *reste*
du travail plateforme, pas la compilation :

1. Chrome natif Windows (menu natif via `windows-rs`, pas `objc2`) et Linux
   (GTK ou juste le menu intégré `egui`, moins critique sur Linux où les
   conventions de menu sont moins strictes qu'sur macOS).
2. Packaging : `cargo-bundle` ne cible qu'macOS — pour Windows, un
   installeur (`cargo-wix` ou NSIS) et une signature de code (certificat
   Authenticode, différent du Developer ID Apple) ; pour Linux, un
   AppImage/Flatpak.
3. Tester réellement sur les deux plateformes (pas seulement "ça compile") —
   CI multi-OS (GitHub Actions matrix `windows-latest`/`ubuntu-latest`) pour
   attraper les régressions de rendu/interaction spécifiques à chaque OS.

**Effort : L** pour un support Windows basique fonctionnel, **XL** pour un
vrai support des trois plateformes au niveau de qualité macOS actuel.

### 10. Maturité produit : mises à jour, crash reporting, support — absent → complet

1. **Mécanisme de mise à jour** : Sparkle (`.appcast` XML + vérification de
   signature) est le standard de facto pour les apps macOS distribuées hors
   App Store — évite de demander à chaque utilisateur de retélécharger le
   `.dmg` manuellement à chaque version.
2. **Rapport de crash** : intégrer un service (Sentry a un SDK Rust) pour
   remonter les panics en production — actuellement, un crash chez un
   utilisateur ne remonte nulle part.
3. **Canal de support** : au minimum, un système d'issues GitHub visible et
   surveillé, documenté dans le README comme point de contact officiel.
4. **Signature/notarisation déjà en place** — voir
   [README.md § Distribution & notarisation](./README.md#distribution--notarisation),
   pas un écart à combler ici.

**Effort : M** pour les trois premiers points, indépendants les uns des
autres.

---

## Ce qui reste volontairement hors de cette feuille de route

- **Signature numérique — production** (pas seulement vérification) :
  nécessiterait de porter une clé privée dans l'app, avec toutes les
  implications de sécurité (stockage de clé, UX de sélection de certificat)
  — un chantier légitime mais distinct, à traiter séparément avec sa propre
  analyse de risque plutôt que comme une ligne parmi d'autres ici.
- **Édition chirurgicale du texte existant** (réécriture réelle du flux de
  contenu avec gestion de sous-ensembles de police, plutôt que superposition) :
  déjà identifié dans `architecture.md` §7.3 comme projet de recherche
  séparé, pas un simple écart de fonctionnalité — la difficulté n'est pas
  le temps de développement mais l'absence d'approche éprouvée simple.

## Ordre de traitement recommandé

1. OCR (P1.1) — débloque le plus grand nombre de cas d'usage réels.
2. Caviardage sécurisé (P1.2) — corrige un piège actif, pas juste un manque.
3. Accessibilité du contenu (P1.3) — la donnée existe déjà, le pont est
   contenu.
4. CCITT (P2.4, sous-partie) — le plus rentable des trois codecs manquants.
5. Reste de P2 et P3 par ordre de section, en parallélisant ce qui est
   indépendant (Type1 et CMaps CJK ne se bloquent pas mutuellement, par
   exemple).
6. P4 en continu, pas comme un jalon isolé — chaque nouvelle fonctionnalité
   livrée devrait idéalement passer par la CI multiplateforme dès qu'elle
   existe (point 9.3), plutôt que d'attendre un "grand chantier Windows"
   séparé.
