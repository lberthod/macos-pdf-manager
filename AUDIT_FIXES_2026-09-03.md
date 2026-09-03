# Corrections et optimisations issues de l'audit du 2026-09-03

Ce document liste les problèmes trouvés lors de l'audit sécurité/qualité du
2026-09-03 (commit `f3c3040`, branche `main`) et les étapes concrètes pour
les corriger. Il complète `AUDIT_FIXES.md` (audit du 2026-08-29, dont les
points #1 à #4 sont corrigés et restent corrigés — vérifié).

**Statut : les 21 points #1-#21 sont corrigés et testés (`cargo fmt`,
`clippy -D warnings`, `cargo test --workspace` verts). Optimisations hors
sécurité : O2 fait, O1 partiellement (piste 1/3, voir sa section), O3
laissé tel quel (suggestions "à confirmer par mesure", pas des actions
concrètes).**
Chaque section se termine par le
test de non-régression à ajouter ; cocher la case du titre quand le code, le
test et `cargo test --workspace` sont verts.

Légende de la colonne « Vérifié » :
- **reproduit** : exécuté sur la machine avec un fichier forgé ou `pdf-cli` ;
- **code** : confirmé par lecture du code, non exécuté ;
- **revue** : relevé par une revue déléguée, vérifié par sondage.

---

## Tableau de bord

| # | Sévérité | Zone | Problème | Vérifié |
|---|----------|------|----------|---------|
| 1 | Critique | pdf-core | Débordement de pile du parseur d'objets (imbrication) | reproduit |
| 2 | Critique | pdf-render, pdf-render-gpu | Aucun plafond sur la taille du rendu (MediaBox forgée) | code |
| 3 | Élevé | pdf-edit | Caviardage : les annotations survivent | reproduit |
| 4 | Élevé | pdf-edit | Caviardage fail-open (`/ActualText`, `sh`, `Do` sans bbox) | code |
| 5 | Élevé | pdf-edit | Extraction/fusion copient tout l'arbre de pages via `/Parent` | reproduit |
| 6 | Élevé | pdf-core, pdf-app | Édition d'un PDF chiffré : perte silencieuse | reproduit |
| 7 | Élevé | pdf-core | Signature : `/ByteRange` non confronté à la taille du fichier | code |
| 8 | Élevé | pdf-ui | Pas d'invite « modifications non enregistrées » | revue |
| 9 | Élevé | CI | `clippy -D warnings` échoue sur le stable actuel | reproduit |
| 10 | Moyen | pdf-edit | Sauvegarde en place non atomique | code |
| 11 | Moyen | pdf-core | Flate sans limite, `/Columns` du prédicteur non borné | revue |
| 12 | Moyen | pdf-ui | Le thread de rendu meurt en silence | revue |
| 13 | Moyen | pdf-ui | Panneau des miniatures non virtualisé | revue |
| 14 | Moyen | pdf-core | Mise à jour incrémentale hybride sur un doc à xref stream | reproduit |
| 15 | Moyen | pdf-core | `/StrF` ignoré au déchiffrement | revue |
| 16 | Moyen | pdf-edit | Caviardage refusé dès que « BI » apparaît dans le flux | code |
| 17 | Faible | outillage | Aucun audit de dépendances | reproduit |
| 18 | Faible | dépôt | LICENSE, `rust-version`, fuzz hors CI, CLI qui écrase | reproduit |
| 19 | Faible | tests | `crypt.rs`, `page.rs`, `pdf-cli` sans tests directs | revue |
| 20 | Faible | pdf-render | Limites de rendu non documentées | revue |
| 21 | Faible | docs | README et ROADMAP surestiment signature et caviardage | code |
| O1 | Optim. | pdf-app | Chaque édition clone et reparse le fichier entier | code |
| O2 | Optim. | pdf-core | Écriture hexadécimale octet par octet via `format!` | code |

---

## Ordre de traitement recommandé

1. **#9** (CI) — sinon rien de ce qui suit n'est vérifié en continu.
2. **#1, #2** (crashs à l'ouverture) — quelques lignes chacun, plus fixtures.
3. **#6** (chiffré) — au minimum bloquer l'édition avec un message clair.
4. **#3, #4, #16** (caviardage) puis **#5** (`/Parent`), qui touche aussi
   extraction, fusion et export optimisé.
5. **#7** (signature) et **#21** (README) ensemble.
6. **#8** (invite de fermeture) et **#10** (sauvegarde atomique) : les deux
   risques de perte de données côté utilisateur.
7. Le reste, par sévérité.

---

## 1. [Critique] Débordement de pile du parseur d'objets — ☑ Corrigé

**Fichier :** `pdf-core/src/parser.rs:121-162` (`parse_array`,
`parse_dict_or_stream`, via `parse_object_from`).

**Problème :** les deux fonctions récursent dans `parse_object_from` pour
chaque élément imbriqué sans compteur de profondeur. Un objet
`[[[[…1…]]]]` à 200 000 niveaux (400 Ko) placé dans le catalogue fait
avorter le processus :

```
thread 'main' has overflowed its stack
fatal runtime error: stack overflow, aborting
```

Ce n'est pas un panic récupérable par `catch_unwind` : l'app entière
disparaît à l'ouverture. L'interpréteur a déjà une limite
(`MAX_XOBJECT_DEPTH` dans `interp.rs`), `document.rs` a un garde de cycle
pour les `ObjStm`, mais le parseur d'objets n'a rien.

**Étapes de correction :**
1. Ajouter un champ `depth: usize` au `Parser` (ou un paramètre à
   `parse_object_from`), incrémenté à l'entrée de `parse_array` /
   `parse_dict_or_stream`, décrémenté à la sortie (garde RAII ou
   décrément explicite sur tous les chemins, y compris erreur).
2. Constante `MAX_OBJECT_DEPTH = 256` (largement au-dessus de tout PDF réel,
   très en dessous de la pile). Au-delà, renvoyer
   `PdfError::InvalidObject("nesting too deep")`.
3. Vérifier que `content.rs` (`parse_content_stream`) passe par le même
   chemin ou a sa propre limite : les opérandes de flux de contenu sont
   aussi des tableaux/dictionnaires imbriquables.
4. Fuzz : le target `parse_document` existant aurait dû le trouver ; le
   corpus initial n'a manifestement jamais atteint cette forme. Ajouter le
   fichier forgé au corpus (`fuzz/corpus/parse_document/`).

**Test de non-régression :** construire en mémoire un PDF minimal dont le
catalogue contient `/X` suivi de 100 000 `[`, `1`, 100 000 `]` ; vérifier que
`Document::open` (ou `doc.root()`) renvoie `Err` et ne panique pas. Le
générateur Python utilisé pour l'audit :

```python
n = 200000
catalog = b"<< /Type /Catalog /Pages 2 0 R /X " + b"[" * n + b"1" + b"]" * n + b" >>"
```

---

## 2. [Critique] Aucun plafond sur la taille du rendu — ☑ Corrigé

**Fichiers :** `pdf-render/src/lib.rs:65-75` (calcul `width`/`height`) puis
`Pixmap::new` ligne 123 ; `pdf-render-gpu/src/lib.rs:168-178` puis
`create_texture` dans `rasterize()` (~ligne 754).

**Problème :** `width = ((mb[2]-mb[0]) * scale).round() as u32` sans borne.
Avec `/MediaBox [0 0 100000 100000]`, tiny-skia fait `vec![0; 40 Go]`
(la doc de `Pixmap::new` admet ne pas pouvoir vérifier l'allocation) ; côté
GPU, la texture dépasse `max_texture_dimension_2d` sans `push_error_scope`
ni `on_uncaptured_error`, ce qui est le chemin de panic par défaut de wgpu.
Atteignable par `pdf-cli render`, par le thread principal de l'UI
(`DocumentTab::ensure_texture`, `main.rs:~1900`) et par le worker de rendu
(`render_worker.rs:161-165`, voir #12). Non exécuté pendant l'audit pour ne
pas saturer la machine.

**Étapes de correction :**
1. Dans `pdf-render`, une constante `MAX_RENDER_PIXELS: u64 = 50_000_000`
   (≈ 7000×7000, suffisant pour A0 à 200 dpi). Avant `Pixmap::new`, si
   `width as u64 * height as u64 > MAX_RENDER_PIXELS`, renvoyer `None` (la
   signature renvoie déjà `Option<Pixmap>`) — ou mieux, réduire `scale`
   pour tenir dans le budget et le signaler à l'appelant.
2. Même garde dans `pdf-render-gpu`, plus une vérification contre
   `device.limits().max_texture_dimension_2d` pour chaque dimension.
3. Dans `pdf-render-gpu`, envelopper `create_texture` dans
   `device.push_error_scope(ErrorFilter::Validation)` /
   `pop_error_scope()` pour transformer l'erreur de validation en `Err`.
4. Côté `pdf-app::Session::render_page`, remonter l'échec comme erreur
   affichable (« page trop grande pour être rendue ») plutôt que de
   propager `None` silencieusement.
5. Le fuzzer `render_document` doit rendre à échelle fixe ; ajouter un cap
   identique pour qu'il ne s'arrête pas sur des OOM.

**Test de non-régression :** fixture `huge_mediabox.pdf` (page unique,
MediaBox 100000×100000) ; `render_page` renvoie `None`/`Err` en moins d'une
seconde et sans allouer plus de quelques Mo. Test à part pour la variante
GPU dans `pdf-render-gpu/tests/cross_backend.rs`.

---

## 3. [Élevé] Caviardage : les annotations survivent — ☑ Corrigé

**Fichier :** `pdf-edit/src/lib.rs:836` (`redact_region`).

**Problème :** `redact_region` ne réécrit que le flux de contenu de la page.
Reproduit avec `pdf-cli` :

```bash
pdf-cli add-text in.pdf a.pdf 0 0 400 612 792 14 SECRETANNOT
pdf-cli redact a.pdf out.pdf 0 0 400 612 792
grep -a -c SECRETANNOT out.pdf   # → 1
```

Le fichier de sortie contient encore l'annotation `/FreeText`, son flux
d'apparence `/AP` et la chaîne, en clair et en hexadécimal. Même problème
pour les widgets de formulaire remplis (`/V`, `/AP`), les superpositions
créées par `replace_text_with_overlay` et les liens. Le scénario est
réaliste : un utilisateur masque d'abord avec « Remplacer… », puis caviarde.

**Étapes de correction :**
1. Dans `redact_region`, après le traitement du flux de contenu, parcourir
   `page_annotation_refs(page_index)` ; pour chaque annotation, lire `/Rect`
   (normaliser les coins) et tester l'intersection avec `target`.
2. Pour toute annotation qui intersecte : la retirer de `/Annots` (réutiliser
   la logique de `remove_annotation`) et, si elle a un `/Parent` de champ
   AcroForm, retirer aussi le widget du champ (`/Kids`) ou vider `/V` et
   `/AP`. Ne pas essayer de « rogner » une annotation : sur-suppression
   assumée, comme pour les XObjects.
3. Consigner toutes ces suppressions dans le même `EditOp` que le
   changement de `/Contents`, pour qu'un `undo` restaure tout d'un bloc.
4. Faire passer `redacted_something` à `true` si au moins une annotation a
   été retirée, même si le flux de contenu n'avait rien dans la zone (un
   utilisateur qui caviarde une note doit y arriver).
5. Documenter dans la doc de module de `redact.rs` que les annotations sont
   traitées par suppression entière.

**Test de non-régression :** dans `pdf-edit`, `add_free_text_annotation`
puis `redact_region` sur la même zone, `save_redacted`, puis :
- scan des octets bruts : ni le texte, ni sa forme hexadécimale ;
- `Document::open` puis `page.annotations()` : aucune annotation dans la
  zone ;
- variante avec `set_form_field_value` sur `acroform_textfield.pdf`.

---

## 4. [Élevé] Caviardage fail-open sur trois opérateurs — ☑ Corrigé

**Fichier :** `pdf-edit/src/redact.rs`, fonction `redact_content_stream`
(bras `_ => {}` de la boucle principale, bras `"Do"`).

**Problème :** le module annonce « prudence par sur-suppression », mais
trois cas font l'inverse :
- **`BDC` avec `/ActualText` ou `/Alt`** : les propriétés de contenu marqué
  (`/Span << /ActualText (texte) >> BDC`) restent dans le flux même si le
  `Tj` qu'elles entourent est retiré. Très courant dans les PDF taggés
  (accessibilité, OCR). Même chose pour les propriétés indirectes via
  `/Properties` des ressources.
- **`sh`** : peint le clip courant avec un ombrage, jamais examiné.
- **`Do` dont `xobject_bbox` renvoie `None`** (XObject introuvable,
  `/BBox` absent, `/Subtype` inattendu) : conservé au lieu de faire échouer
  l'opération.

**Étapes de correction :**
1. Suivre une pile `BDC`/`EMC` : si un opérateur de peinture retiré se
   trouve dans un bloc marqué dont le dictionnaire de propriétés (inline ou
   via `/Properties`) contient `/ActualText`, `/Alt`, `/E` ou `/Lang`,
   retirer le `BDC` et son `EMC` (ou réécrire le `BDC` sans ces clés).
   Par simplicité : retirer le couple entier quand n'importe quoi à
   l'intérieur a été retiré.
2. Traiter `sh` comme un remplissage de la boîte du clip courant ; sans
   suivi du clip (documenté), utiliser la boîte de la page → toujours retiré
   si la zone est non vide. Sur-suppression, cohérent avec le reste.
3. Pour `Do` : si `xobject_bbox` renvoie `None`, renvoyer
   `Err("XObject /Name : boîte inconnue, caviardage refusé")`.
4. Ajouter `d0`/`d1`, `BI` (voir #16) et `Tr` à la liste des cas documentés.

**Tests de non-régression (dans `redact.rs`) :**
- `/Span << /ActualText (secret) >> BDC BT … (x) Tj ET EMC` : après
  caviardage, `secret` absent du flux ;
- `q 0 0 100 100 re W n /Sh0 sh Q` avec zone qui intersecte : `sh` retiré ;
- `/Missing Do` : `Err`, pas `Ok((…, false))`.

---

## 5. [Élevé] Extraction et fusion copient tout l'arbre de pages — ☑ Corrigé

**Fichiers :** `pdf-edit/src/lib.rs:2226` (`extract_pages`), `:2018`
(`copy_page`, utilisé par `merge_document` et `insert_pages_from`),
`:2160` (`copy_object_recursive`).

**Problème :** `copy_object_recursive` est appelé sur le dictionnaire brut de
la page **avant** que `/Parent` soit remplacé. Il suit donc `/Parent` →
nœud `/Pages` → `/Kids` → toutes les pages du document source, avec leurs
contenus et ressources. Reproduit :

```bash
pdf-cli split multipage_classic_xref.pdf out.pdf 0
grep -a -o '/Type /Page\b' out.pdf | wc -l   # → 5 pour 1 page extraite
```

Conséquences : un « extrait » partagé contient les pages non extraites
(fuite), `export_optimized` (donc `save_redacted`, #3) ne fait pas le
ramasse-miettes promis, et chaque fusion embarque l'intégralité de l'autre
document. Le test `merge_document_appends_real_pages_with_their_resources`
ne vérifie que le nombre de pages, ce qui a laissé passer le problème.

**Étapes de correction :**
1. Dans `extract_pages` et `copy_page` : cloner le dictionnaire de la page,
   retirer `/Parent` (et `/StructParents`, `/B` beads), **puis** appeler
   `copy_object_recursive`.
2. Les annotations copiées ont un `/P` (page parente) et parfois un
   `/Popup`/`/Parent` (champ) : `/P` pointe vers la page en cours de copie,
   déjà dans `copy_map`, donc pas de fuite ; mais un `/Parent` de champ
   AcroForm peut remonter vers d'autres pages via `/Kids` — retirer
   `/Parent` des widgets copiés ou limiter la copie des `/Kids` aux widgets
   de la page.
3. Après correctif, vérifier `save_redacted` (#3) : l'ancien flux de contenu
   ne doit toujours pas être atteignable.

**Tests de non-régression :**
- `extract_pages(&doc, &[0])` sur `multipage_classic_xref.pdf` : exactement
  un objet `/Type /Page` dans la sortie et nombre total d'objets < 10 ;
- `merge_document` : nombre d'objets de la sortie ≤ objets(base) +
  objets atteignables(source) ;
- réutiliser le scan d'octets bruts du test de caviardage sur une page
  extraite pour vérifier qu'aucun texte des autres pages n'est présent
  (avec `export_optimized`, les flux copiés gardent leur compression : tester
  plutôt via `Document::open` + comptage de pages atteignables **et**
  d'objets totaux).

---

## 6. [Élevé] Édition d'un PDF chiffré : perte silencieuse — ☑ Corrigé (temps 1)

**Statut :** temps 1 (refus explicite) implémenté et testé. Le temps 2
(rechiffrement réel des objets ajoutés, cf. options (a)/(b) ci-dessous)
reste à faire — voir la case laissée ouverte dans le corps de la section.

**Fichiers :** `pdf-core/src/document.rs:248` (`save_incremental`),
`pdf-core/src/xref.rs:69-74` (fusion des trailers de la chaîne `/Prev`),
`pdf-app/src/lib.rs:934` (`refresh_after_edit`).

**Problème :** la sauvegarde incrémentale écrit les nouveaux objets **en
clair** et le trailer ajouté ne contient ni `/Encrypt` ni `/ID`. Comme la
lecture fusionne les trailers de toute la chaîne, le fichier reste vu comme
chiffré et les objets en clair sont « déchiffrés » en bruit. Reproduit :

```bash
pdf-cli highlight encrypted_rc4.pdf out.pdf 0 50 50 300 100
pdf-cli render encrypted_rc4.pdf a.png 0 ; pdf-cli render out.pdf b.png 0
cmp a.png b.png   # identiques : le surlignage a disparu
```

Le même test sur `minimal.pdf` montre bien le surlignage. Avec un mot de
passe utilisateur non vide, c'est pire : `refresh_after_edit` rouvre les
octets avec `Document::open` (sans mot de passe) → chaque édition renvoie
« mot de passe incorrect ».

**Étapes de correction (deux temps) :**

*Temps 1, immédiat — refuser proprement :*
1. Exposer `Document::is_encrypted()` (vrai si `decryptor.is_some()`).
2. Dans `EditSession::open_with_password` (ou dans chaque opération de
   `commit`), renvoyer `Err("l'édition d'un PDF chiffré n'est pas encore
   prise en charge ; exportez d'abord une copie déchiffrée")`.
3. Dans `pdf-ui`, désactiver les outils d'édition sur un onglet chiffré
   (le viewer reste fonctionnel).

*Temps 2 — support réel, au choix :*
- **(a) Réécriture complète déchiffrée** : après authentification,
  `export_optimized` sans `/Encrypt` (perd la protection, à afficher
  clairement). Simple, mais change la nature du fichier.
- **(b) Rechiffrement des objets ajoutés** : `save_incremental` reçoit le
  `Decryptor`, chiffre chaînes et flux des objets `pending` avec la clé
  d'objet (même algorithme que le déchiffrement, RC4/AES symétriques),
  recopie `/Encrypt` et `/ID` dans le trailer. Préserve le fichier ; demande
  un `Encryptor` miroir dans `crypt.rs` (~100 lignes) et des tests.

Quel que soit le choix, `refresh_after_edit` doit conserver le mot de passe
(`Session` garde `password: Vec<u8>` et rouvre avec
`Document::open_with_password`).

**Tests de non-régression :** sur les trois fixtures chiffrées
(`encrypted_rc4.pdf`, `encrypted_aes256.pdf`, `encrypted_user_password.pdf`) :
- temps 1 : `Session::add_highlight…` renvoie `Err` explicite ;
- temps 2 : surlignage ajouté → sauvegardé → rouvert avec mot de passe →
  `page.annotations()` contient le surlignage et le rendu diffère de
  l'original.

---

## 7. [Élevé] Signature : `/ByteRange` non confronté à la taille du fichier — ☑ Corrigé

**Fichier :** `pdf-core/src/signature.rs:196-215` (lecture de `/ByteRange`),
`:270-280` (hachage des deux plages).

**Problème :** seules les deux paires (offset, longueur) sont hachées. Rien
n'exige que la première plage commence à 0, que le trou entre les deux
corresponde exactement à la chaîne hexadécimale `/Contents`, ni que la
seconde plage se termine à `document_bytes.len()`. Une mise à jour
incrémentale ajoutée après une signature valide (page modifiée, champ,
JavaScript) est affichée `Status: VALID` par `pdf-cli verify-signatures`
et par l'UI. C'est l'attaque classique « incremental saving attack ». Le
README annonce pourtant la « détection d'une modification après signature ».

**Étapes de correction :**
1. Après lecture de `nums = [a0, l0, a1, l1]`, exiger : `a0 == 0`,
   `a0 + l0 <= a1`, `a1 + l1 == document_bytes.len()`. Sinon, nouveau
   statut `SignatureStatus::CoversPartialFile` (ou `ModifiedAfterSigning`
   quand `a1 + l1 < len`).
2. Vérifier que le trou `[a0+l0, a1)` contient exactement `/Contents` :
   localiser la valeur de `/Contents` dans les octets bruts du dictionnaire
   de signature (le parseur ne conserve pas les offsets ; une recherche de
   `<…>` entre `a0+l0` et `a1` qui ne contient que des chiffres hexa et des
   blancs suffit) et refuser tout autre octet significatif dans le trou.
3. Tolérer le cas légitime d'un fichier terminé par `%%EOF` suivi d'un ou
   deux octets de fin de ligne (`\r\n`) : accepter `a1 + l1 >= len - 2` si
   le reste n'est que des blancs.
4. Faire remonter les nouveaux statuts dans `pdf-cli` (« signée, puis
   modifiée ») et dans l'UI.

**Tests de non-régression :** à partir de `signed_document.pdf` :
- ajouter une mise à jour incrémentale (via `save_incremental` d'un objet
  quelconque) → statut `ModifiedAfterSigning`, pas `Valid` ;
- forger un `/ByteRange` qui saute 100 octets au milieu → `CoversPartialFile` ;
- le fichier d'origine reste `Valid`.

---

## 8. [Élevé] Pas d'invite « modifications non enregistrées » — ☑ Corrigé

**Fichiers :** `pdf-ui/src/main.rs:3576-3600` (`close_active_tab`,
`close_tab`), `impl eframe::App for ViewerApp` (`main.rs:3658`, seul
`update()` est implémenté), `native_menu.rs:213` (Quitter → `terminate:`).

**Problème :** Cmd+W retire l'onglet sans condition ; Cmd+Q passe
directement par `terminate:` de la chaîne de répondeurs. Annotations,
valeurs de formulaire, réorganisations de pages en attente sont perdues
sans avertissement. `Session::can_undo_edit()` existe déjà et suffit comme
indicateur « document modifié ».

**Étapes de correction :**
1. Ajouter `Session::is_dirty()` : vrai si la pile d'undo n'est pas vide
   **depuis la dernière sauvegarde** (mémoriser la longueur de la pile au
   moment de `save_edits_in_place`).
2. Fermeture d'onglet : si `is_dirty()`, ouvrir une modale egui
   « Enregistrer / Ne pas enregistrer / Annuler » ; ne retirer l'onglet
   qu'après réponse.
3. Fermeture de fenêtre / Quitter : dans `update()`, tester
   `ctx.input(|i| i.viewport().close_requested())` ; si un onglet est sale,
   `ctx.send_viewport_cmd(ViewportCommand::CancelClose)` et ouvrir la même
   modale (boucle sur les onglets sales). Pour Cmd+Q, remplacer l'action
   `terminate:` du menu natif par une action applicative qui passe par le
   même chemin, ou implémenter `applicationShouldTerminate:` sur le délégué.
4. Marquer l'onglet sale dans son titre (« • nom.pdf »), convention macOS.

**Test de non-régression :** `pdf-app` : `is_dirty()` faux à l'ouverture,
vrai après une édition, faux après `save_edits_in_place`, vrai à nouveau
après `undo` d'une édition sauvegardée. Le comportement UI se vérifie à la
main (pas de test egui en place).

---

## 9. [Élevé] CI rouge : `clippy -D warnings` échoue sur le stable actuel — ☑ Corrigé

**Fichiers :** `.github/workflows/ci.yml` (`dtolnay/rust-toolchain@stable`,
`cargo clippy --workspace --all-targets -- -D warnings`).

**Problème :** avec rustc 1.98 (stable au 2026-09-03), 9 avertissements
`using chunks_exact with a constant chunk size` :

```
pdf-core/src/interp.rs:1361
pdf-core/src/font.rs:303, 445, 762, 807, 895
pdf-core/src/object.rs:127
pdf-render/src/lib.rs:317, 321
```

`cargo clippy --workspace --all-targets -- -D warnings` sort en code 101 :
tout push ou PR est actuellement en échec. En bonus, cargo signale
`block v0.1.6` (via `metal` ← `wgpu-hal` 22) comme future-incompatible.

**Étapes de correction :**
1. `cargo clippy --fix --workspace --all-targets --allow-dirty` puis relire
   le diff (remplacement par `as_chunks::<N>()`), `cargo test --workspace`.
2. Ajouter un `rust-toolchain.toml` (`channel = "1.98"`) ou `rust-version`
   dans `[workspace.package]` et faire pointer la CI sur cette version pour
   qu'un nouveau stable ne casse plus la CI sans action de ta part ; prévoir
   une mise à jour trimestrielle.
3. Ajouter un job CI `cargo audit` (voir #17).
4. Planifier la montée `wgpu 22 → 24+` / `eframe 0.29 → 0.31+` pour sortir
   `block 0.1.6` (le futur rustc refusera de le compiler).

**Vérification :** CI verte sur `main`.

---

## 10. [Moyen] Sauvegarde en place non atomique — ☑ Corrigé

**Fichiers :** `pdf-edit/src/lib.rs:2078` (`save_as`), `:2100`
(`save_redacted`), appelés par `pdf-app/src/lib.rs:899`
(`save_edits_in_place`) sur `self.path`.

**Problème :** `std::fs::write(path, …)` tronque puis écrit. Un crash, un
kill ou un disque plein pendant l'écriture laisse le PDF d'origine tronqué
(et il n'y a pas de copie : la session ne garde que les octets en mémoire).

**Étapes de correction :**
1. Fonction utilitaire `write_atomic(path, bytes)` : écrire dans
   `path.with_extension("pdf.tmp-<pid>")` dans le même répertoire, `sync_all`,
   puis `std::fs::rename` (atomique sur APFS). Copier les permissions du
   fichier d'origine (`fs::metadata(path)?.permissions()`).
2. L'utiliser dans `save_as`, `save_redacted`, `extract_pages_to_file`,
   `export_*` de `pdf-ui` et les commandes CLI.
3. Cas du fichier ouvert dans deux onglets : après sauvegarde, les autres
   onglets sur le même chemin ont un `Document` périmé — au minimum
   afficher un bandeau « modifié sur le disque, recharger ? ».

**Test de non-régression :** `pdf-edit` : après `save_as` sur un chemin
existant, aucun fichier `*.tmp-*` ne subsiste et les permissions sont
inchangées ; simuler un échec (chemin dont le répertoire est en lecture
seule) et vérifier que l'original est intact.

---

## 11. [Moyen] Flate sans limite et `/Columns` non borné — ☑ Corrigé

**Fichiers :** `pdf-core/src/filters.rs:56-63` (`flate_decode`,
`read_to_end`), `:38-54` (`decode_stream`, chaîne de filtres), `:256-298`
(`apply_predictor` / `png_predictor`, `vec![0u8; row_bytes]`).

**Problème :** une bombe Flate (éventuellement chaînée `[/FlateDecode
/FlateDecode]`) décompresse plusieurs Go à partir de quelques Ko. Pour le
prédicteur, `row_bytes` dérive de `/Columns × /Colors × /BitsPerComponent`
sans borne ; l'essai avec `/Columns 2000000000` et un flux vide n'a pas
planté sur macOS (allocation paresseuse, 3,7 Mo résidents), mais un
environnement sans surallocation abortera, et `columns * bits_per_pixel`
peut déborder en debug.

**Étapes de correction :**
1. `flate_decode` : lire avec `take(MAX_DECODED_LEN)` (256 Mo) et renvoyer
   `PdfError::DecodeError("stream too large")` si la limite est atteinte.
   Appliquer la même limite à la sortie de chaque étage de la chaîne.
2. `apply_predictor` : refuser `colors > 64`, `bpc ∉ {1,2,4,8,16}`,
   `columns > 1 << 20`, et utiliser `checked_mul`.
3. Même revue pour `image.rs:52-99` (`width × height × composants` avec
   `checked_mul`, refuser > `MAX_IMAGE_PIXELS`).

**Tests de non-régression :** flux Flate de 1 Mo décompressant en 1 Go →
`Err` en moins d'une seconde ; `/Columns 1 << 40` → `Err` sans allocation.

---

## 12. [Moyen] Le thread de rendu meurt en silence — ☑ Corrigé

**Fichier :** `pdf-ui/src/render_worker.rs:126-178` (`worker_loop`) ;
envois en `let _ = sender.send(...)` lignes 91 et 106.

**Problème :** aucun `catch_unwind`, aucun redémarrage. Un panic dans le
worker (déclencheur direct : #2) fige miniatures et défilement continu de
l'onglet sans message ; les `send` suivants échouent silencieusement.

**Étapes de correction :**
1. Envelopper chaque tâche dans `std::panic::catch_unwind(AssertUnwindSafe(…))`
   et renvoyer un message `RenderResult::Failed { page, reason }` à l'UI.
2. Si le thread meurt malgré tout (`send` renvoie `Err`), le recréer à la
   demande suivante et afficher une erreur dans `self.error`.
3. Le rendu du thread principal (`ensure_texture`) doit aussi rattraper un
   `None` de #2 et afficher une page grise avec message.

**Test :** difficile à tester automatiquement ; vérifier à la main avec la
fixture `huge_mediabox.pdf` (#2) une fois le cap en place : l'UI affiche une
erreur, les autres pages continuent de se rendre.

---

## 13. [Moyen] Panneau des miniatures non virtualisé — ☑ Corrigé

**Fichier :** `pdf-ui/src/main.rs:2038-2043` (`show_thumbnail_panel`).

**Problème :** `for index in 0..page_count { ensure_thumbnail(index) }`
dans un `ScrollArea::vertical()` simple : sur un document de plusieurs
milliers de pages, un rendu est lancé pour chaque page et toutes les
`TextureHandle` sont conservées (`self.thumbnails`, vidé seulement à
l'ouverture ou à une édition). Le défilement continu (`show_rows`, ligne
~2545) fait déjà les choses correctement.

**Étapes de correction :**
1. Utiliser `ScrollArea::vertical().show_rows(ui, row_height, page_count, …)`
   comme dans `show_continuous_scroll` ; n'appeler `ensure_thumbnail` que
   pour les lignes visibles.
2. Cache LRU borné (ex. 200 miniatures) : retirer les textures les plus
   anciennes hors de la fenêtre visible.
3. Même revue pour le cache de rendu par page/zoom (`render_cache` de
   `pdf-app`) : borner en Mo plutôt qu'en entrées.

**Test :** ouvrir `large_60_pages.pdf` avec le panneau ouvert et vérifier
(compteur de debug) que ≤ 15 rendus de miniatures sont demandés au premier
affichage.

---

## 14. [Moyen] Mise à jour incrémentale hybride sur un doc à xref stream — ☑ Corrigé

**Fichier :** `pdf-core/src/document.rs:248` (`save_incremental`, écrit
toujours une table `xref` classique).

**Problème :** reproduit sur `multipage_xref_stream.pdf` : après
`highlight`, la sortie a une table classique dont `/Prev` pointe vers un
flux xref. Notre parseur l'accepte, mais ISO 32000-1 §7.5.8 attend un flux
xref pour les mises à jour d'un tel fichier ; des lecteurs stricts refusent
ou reconstruisent. Le trailer ajouté omet aussi `/ID` et `/Info` (tolérés
par fusion, mais `/ID` doit être présent et son second élément mis à jour).

**Étapes de correction :**
1. Mémoriser dans `Document` si la dernière section lue était un flux xref
   (`XrefKind::Stream`).
2. Dans `save_incremental`, si `XrefKind::Stream` : écrire un objet
   `/Type /XRef` avec `/W [1 4 2]`, `/Index`, `/Size`, `/Prev`, `/Root`,
   `/ID`, `/Info`, flux non compressé (pas de `/Filter`, plus simple et
   valide).
3. Dans les deux cas, recopier `/ID` (en régénérant le second élément, par
   exemple MD5 des octets écrits) et `/Info` depuis le trailer fusionné.
4. Réutiliser `write_standalone` pour factoriser l'écriture de la xref.

**Test de non-régression :** sauvegarde incrémentale sur
`multipage_xref_stream.pdf` → la dernière section est un flux xref
(`startxref` pointe sur `N 0 obj << /Type /XRef`), réouverture OK,
`trailer.get("ID")` présent.

---

## 15. [Moyen] `/StrF` ignoré au déchiffrement — ☑ Corrigé

**Fichier :** `pdf-core/src/crypt.rs:322-339` (`crypt_filter_method`),
`:263-274` (application au flux et aux chaînes).

**Problème :** seul `/StmF` est lu, et sa méthode est appliquée aux
chaînes comme aux flux. Un PDF conforme avec `StrF Identity` /
`StmF AESV2` voit ses chaînes mal déchiffrées sans erreur (métadonnées,
`/Contents` d'annotations en bruit). Le commentaire du code l'assume.

**Étapes de correction :**
1. Lire `/StrF` séparément (défaut `Identity` comme `/StmF`), résoudre son
   filtre dans `/CF`, stocker `string_method` et `stream_method` dans
   `Decryptor`.
2. `decrypt_string` utilise `string_method`, `decrypt_stream` utilise
   `stream_method` ; respecter aussi `/EncryptMetadata false` pour le flux
   `/Metadata`.
3. Fixture : générer avec qpdf (`--encrypt … --use-aes=y` puis éditer
   `/StrF /Identity` à la main) un PDF où les deux diffèrent.

**Test de non-régression :** fixture ci-dessus → texte extrait correct et
`/Info /Title` lisible.

---

## 16. [Moyen] Caviardage refusé dès que « BI » apparaît — ☑ Corrigé

**Fichier :** `pdf-edit/src/redact.rs`, début de `redact_content_stream`
(`content.windows(2).any(|w| w == b"BI")`).

**Problème :** `(BIENVENUE) Tj`, `/GS0BI gs` ou un flux binaire de police
Type3 suffisent à bloquer l'opération avec le message « images en ligne ».
Le flux est parsé juste après ; la détection sur octets bruts est inutile.

**Étapes de correction :**
1. Vérifier que `parse_content_stream` produit une instruction pour `BI`
   (opérateur `BI` avec les données jusqu'à `EI` en opérande) ; sinon,
   l'ajouter au parseur de contenu (nécessaire de toute façon pour le rendu).
2. Remplacer le scan d'octets par `instructions.iter().any(|i| i.operator == "BI")`.
3. Étape suivante (facultative) : traiter `BI` comme une image de boîte
   unitaire sous la CTM courante, retirée entière si elle intersecte.

**Test de non-régression :** `BT /F1 12 Tf 10 700 Td (BIENVENUE) Tj ET` hors
zone → `Ok((…, false))` et non `Err`.

---

## 17. [Faible] Aucun audit de dépendances — ☑ Corrigé

**Problème :** `cargo audit` et `cargo deny` ne sont installés ni
localement ni en CI. Points connus à surveiller : `cms 0.3.0-pre.2`
(pré-version, tire `der`/`spki` 0.8 à côté de 0.7), `rsa 0.9`
(RUSTSEC-2023-0071 « Marvin », sans impact ici car seule la vérification est
utilisée), `block 0.1.6` non maintenu (voir #9).

**Étapes :**
1. `cargo install cargo-audit cargo-deny`, `cargo audit`, `cargo deny init`
   puis `cargo deny check` ; documenter les exceptions justifiées dans
   `deny.toml` (`ignore = ["RUSTSEC-2023-0071"]` avec raison).
2. Job CI `audit` (`rustsec/audit-check` ou `cargo audit` direct), non
   bloquant les deux premières semaines, puis bloquant.
3. Surveiller la sortie d'une version stable de `cms` et y passer.

---

## 18. [Faible] Hygiène de dépôt — ☑ Corrigé

- **`LICENSE` absent** alors que `Cargo.toml` déclare `license = "MIT"` :
  ajouter le fichier MIT au nom de Loïc Berthod, année 2026.
- **`rust-version` absent** : voir #9.
- **Fuzz jamais exécuté** : ajouter un job CI hebdomadaire
  (`cargo +nightly fuzz run parse_document -- -max_total_time=600`) et
  committer un corpus minimal incluant les fixtures forgées de #1, #2, #11.
- **CLI écrase les sorties** sans avertissement (`save_as`, `fs::write` dans
  `pdf-cli/src/main.rs`) : refuser si `out` existe sauf `--force`, ou au
  moins l'indiquer dans `print_usage`.

---

## 19. [Faible] Trous de couverture — ☑ Corrigé (crypt, page, pdf-cli)

Comptes approximatifs de `#[test]` par module de `pdf-core` : lexer 7,
parser 6, xref 2, document 19, filters 8, **crypt 0**, signature 4,
font 17, interp 15, image 4, encoding 3, content 5, **page 0**, outline 3.
`pdf-cli` : 0.

**Étapes :**
1. `crypt.rs` : tests unitaires directs pour RC4 (V2), AES-128 (V4), R5,
   R6, mauvais mot de passe → `IncorrectPassword`, `/EncryptMetadata false`,
   `/StrF ≠ /StmF` (#15).
2. `page.rs` : héritage `/Resources`, `/MediaBox`, `/Rotate` sur trois
   niveaux, cycle dans `/Kids`, `/Count` incohérent.
3. `pdf-cli` : tests d'intégration `assert_cmd`-like (ou `std::process`) sur
   `split`, `redact`, `verify-signatures` avec codes de sortie.
4. Renforcer `merge_document_appends_real_pages_with_their_resources` avec
   un comptage d'objets (#5).

---

## 20. [Faible] Limites de rendu non documentées — ☑ Corrigé

**Fichier :** `pdf-render/src/lib.rs` (doc de module), `DisplayItem` dans
`pdf-core/src/display.rs`.

Pas de modes de fusion (uniquement source-over), pas de pointillés
(`Stroke { width, ..Default::default() }` ligne ~184, jamais de dash), pas
d'ombrages ni de motifs (`DisplayItem` n'a pas de variante), modes de rendu
de texte (`Tr` 3/7 : invisible/clip) non distingués — le texte invisible
d'un OCR est peint. La doc de module cite CMYK, ICC et les codecs manquants
mais pas ceux-ci.

**Étapes :** compléter la doc de module et le README (section « Ce qui ne
fonctionne pas encore »). Le mode `Tr 3` (texte invisible, cas OCR très
courant) mérite d'être implémenté en premier : ajouter `render_mode` à
`DisplayItem::Glyph` et ne pas peindre quand il vaut 3 ou 7.

---

## 21. [Faible] Documentation qui surestime — ☑ Corrigé

- **README, section Fonctionnalités** : « détection d'une modification
  après signature » est faux pour une mise à jour incrémentale tant que #7
  n'est pas corrigé. Reformuler : « intégrité des octets couverts par
  `/ByteRange` ; la détection d'ajouts postérieurs est en cours ».
- **ROADMAP_IMPROVEMENT.md §2** titre « absent → complet (métadonnées/XMP en
  suspens) » : rétrograder en « partiel » tant que #3 et #4 sont ouverts, et
  lister explicitement : annotations, contenu marqué, `sh`, images en
  ligne, métadonnées.
- **README** : mentionner que l'édition des PDF chiffrés n'est pas prise en
  charge (#6).

**Statut :** #7 étant désormais corrigé, la phrase « détection d'une
modification après signature » du README est redevenue exacte (pas de
reformulation nécessaire). #3 et #4 étant corrigés, `ROADMAP_IMPROVEMENT.md`
§2 liste maintenant précisément ce qui reste ouvert (métadonnées/XMP, images
en ligne) plutôt que la liste plus large d'avant ces correctifs. Le README
mentionne désormais explicitement que l'édition d'un PDF chiffré n'est pas
prise en charge (#6).

---

## Optimisations (hors sécurité)

### O1. Chaque édition clone et reparse le fichier entier — ☑ Piste 1 faite

**Fichiers :** `pdf-app/src/lib.rs:933` (`refresh_after_edit`),
`pdf-edit/src/lib.rs:2107` (`to_bytes`), `pdf-core/src/document.rs:248`
(`save_incremental`, `self.data.clone()`).

**Problème :** après chaque opération (un surlignage, un déplacement
d'annotation), `to_bytes` clone les octets du fichier d'origine, y ajoute
tous les objets en attente, puis `Document::open` reparse le tout et tous
les caches sont vidés. Sur un PDF de 100 Mo, chaque clic coûte une copie de
100 Mo et un reparse complet. `Session` conserve en plus deux copies des
octets (`doc.data` et `edit.doc.data`).

**Pistes :**
1. Partager les octets d'origine en `Arc<[u8]>` entre `Session::doc` et
   `EditSession::doc` ; `save_incremental` écrit dans un `Vec` préalloué à
   `data.len() + estimation` sans cloner deux fois.
2. Ne pas rouvrir le document : appliquer les objets `pending` comme une
   couche « overlay » dans `Document::resolve` (un `HashMap<u32, Object>`
   consulté avant la xref). `Document::open` n'est alors nécessaire qu'à la
   sauvegarde. C'est le vrai gain, et il simplifie #6.
3. Invalider les caches par page touchée plutôt que globalement
   (`EditOp` connaît la page).

**Mesure :** `pdf-cli bench` sur `large_60_pages.pdf` avant/après, temps
d'un `add_highlight_annotation` + `refresh_after_edit`.

**Statut :** piste 1 faite — `pdf_core::Document` porte désormais `Arc<[u8]>`
au lieu de `Vec<u8>` (`open`/`open_with_password` acceptent `impl
Into<Arc<[u8]>>`), et `pdf_app::Session::open_with_password` lit le fichier
une seule fois puis partage cette référence entre son propre `Document` et
celui d'`EditSession` (nouveau constructeur `EditSession::from_bytes`) : les
deux copies complètes des octets en mémoire à l'ouverture n'en font plus
qu'une. Testé (`Document::open` partage bien la même allocation via
`Arc::strong_count`, voir
`pdf-core/src/document.rs::open_shares_the_underlying_bytes_via_arc_rather_than_copying`).
Pistes 2 (overlay dans `Document::resolve`, "le vrai gain" pour éviter un
reparse complet à *chaque* édition) et 3 (invalidation de cache par page)
non faites — piste 2 en particulier est une réécriture plus large,
délibérément laissée pour une passe séparée plutôt que risquée dans celle-ci
(optimisation hors périmètre sécurité).

### O2. Écriture hexadécimale octet par octet via `format!` — ☑ Fait

**Fichier :** `pdf-core/src/writer.rs:84-95` (`write_hex_string`), `:67-82`
(`write_name`).

**Problème :** un `format!("{b:02X}")` par octet, avec allocation d'une
`String` à chaque fois. Sensible pour les flux d'apparence et les chaînes
UTF-16 longues, et pour `export_optimized` qui réécrit tout.

**Piste :** table `HEX: [u8; 16]` et `out.push(HEX[(b >> 4) as usize]);
out.push(HEX[(b & 15) as usize]);`. Même chose pour `#XX` dans
`write_name`. Trivial, sans risque, testé par les tests existants du writer.

### O3. Divers, à confirmer par mesure

- `Rect::from_points` et `text_run_bbox` dans `redact.rs` recalculent la
  police via `resolve_font` avec un cache par nom : correct, mais
  `Font::load` reste coûteux ; partager le cache de polices de
  l'interpréteur si celui-ci en expose un.
- `show_thumbnail_panel` (#13) et le cache de rendu par zoom : borner en
  mémoire, pas en entrées.

---

## Fixtures forgées à ajouter à `pdf-core/tests/fixtures/`

Générées pendant l'audit (script Python ci-dessous), à committer avec une
entrée dans `fixtures/README.md` :

| Fichier | Sert à | Attendu |
|---------|--------|---------|
| `deep_nesting.pdf` | #1 | `Document::open` → `Err`, pas de crash |
| `huge_mediabox.pdf` | #2, #12 | `render_page` → `None`/`Err` rapide |
| `predictor_huge_columns.pdf` | #11 | `page_content` → `Err` sans allocation |
| `signed_then_modified.pdf` | #7 | `ModifiedAfterSigning` |
| `encrypted_strf_identity.pdf` | #15 | texte et `/Info` lisibles |

```python
import zlib

def build(objs, root=1):
    out = b"%PDF-1.7\n"; offs = []
    for i, o in enumerate(objs, 1):
        offs.append(len(out)); out += f"{i} 0 obj\n".encode() + o + b"\nendobj\n"
    x = len(out)
    out += f"xref\n0 {len(objs)+1}\n0000000000 65535 f \n".encode()
    for o in offs: out += f"{o:010} 00000 n \n".encode()
    out += f"trailer\n<< /Size {len(objs)+1} /Root {root} 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()
    return out

n = 200000
deep = build([b"<< /Type /Catalog /Pages 2 0 R /X " + b"[" * n + b"1" + b"]" * n + b" >>",
              b"<< /Type /Pages /Kids [] /Count 0 >>"])

huge = build([b"<< /Type /Catalog /Pages 2 0 R >>",
              b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
              b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100000 100000] >>"])

data = zlib.compress(b"")
pred = build([b"<< /Type /Catalog /Pages 2 0 R >>",
              b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
              b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
              b"<< /Length " + str(len(data)).encode()
              + b" /Filter /FlateDecode /DecodeParms << /Predictor 15 /Columns 1099511627776 >> >>\nstream\n"
              + data + b"\nendstream"])
```

---

## Ce qui tient bien (à ne pas casser)

- Discipline d'erreurs : une vingtaine d'`unwrap` hors tests sur ~23 000
  lignes, tous protégés par un invariant local.
- Le seul `unsafe` (`pdf-ui/src/native_menu.rs`) est sain : cible retenue
  via `Retained`, `MainThreadMarker` exigé partout.
- Invalidation de cache correcte dans l'UI (générations de rendu).
- Les DoS de l'audit du 2026-08-29 sont réellement corrigés avec fixtures.
- Le test de caviardage existant scanne les octets bruts du fichier : c'est
  le bon modèle, à étendre (#3, #5).
