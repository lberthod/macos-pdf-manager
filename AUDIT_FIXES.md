# Corrections issues de l'audit (2026-08-29)

Ce document liste les problèmes trouvés lors de l'audit de sécurité/qualité du
2026-08-29 et les étapes concrètes pour les corriger. Remplace les anciens
documents de suivi de sprint (`STATUS.md`, `sprint.md`, `audit50quest.md`,
`analyse_sprint.md`, `UIX_ANALYSE.md`), supprimés car obsolètes/redondants
avec cet audit et l'historique git.

**Statut : les points #1 à #4 sont corrigés** (voir détail de chacun ci-dessous
— code modifié, tests de régression ajoutés, `cargo test --workspace` (238
tests) et `cargo fmt --check` verts, aucun nouveau warning clippy introduit).
Seul #5 reste une surveillance passive, sans action de code possible.

---

## 1. [Sévérité élevée] Récursion infinie dans `Document::resolve` — DoS — ✅ Corrigé

**Fichier :** `pdf-core/src/document.rs:107-178`

**Problème :** `resolve()` et `resolve_compressed()` n'ont aucune détection
de cycle. Un xref stream forgé qui déclare l'objet N comme
`Compressed { stream_num: N, index: 0 }` (auto-référence), ou un cycle
A→B→A, fait boucler `resolve → resolve_compressed → resolve → …`
indéfiniment jusqu'au stack overflow (crash du process, pas une erreur
`Result` récupérable). Le cache n'empêche rien ici car il n'est rempli
qu'après le retour de l'appel `resolve` initial.

**Étapes de correction :**
1. Ajouter un `RefCell<HashSet<u32>>` (ou équivalent) sur `Document` pour
   suivre les `stream_num` en cours de résolution — sur le modèle de
   `visited` dans `xref::parse_xref_chain`.
2. Dans `resolve()`, avant d'appeler `resolve_compressed(stream_num, index)`,
   vérifier que `stream_num` n'est pas déjà dans l'ensemble "en cours" ; si
   oui, renvoyer une erreur (`PdfError::InvalidObject` ou nouvelle variante
   `PdfError::CyclicReference`) plutôt que de récurser.
3. Insérer `stream_num` dans l'ensemble avant l'appel récursif, le retirer
   après (y compris en cas d'erreur — utiliser un garde RAII ou un
   `finally`-like pattern).
4. Ajouter un test de régression : un fixture PDF (ou construit à la main en
   mémoire) avec un objet auto-référencé dans un xref stream, vérifier que
   `Document::open`/`resolve` renvoie une erreur propre au lieu de crasher.

---

## 2. [Mineur] Cast `i64 as usize` non validé sur `/W` dans le xref stream — ✅ Corrigé

**Fichier :** `pdf-core/src/xref.rs:194` (`parse_xref_stream_section`)

**Problème :** `o.as_int().unwrap_or(0) as usize` — une largeur négative dans
`/W` s'enroule en une valeur énorme lors du cast, et `w0+w1+w2` peut alors
déborder. Sans effet en release (la boucle s'arrête proprement dès que
`cursor+record_len > decoded.len()`), mais panique en build debug/fuzz
(mêmes symptômes que les 2 bugs déjà corrigés au Sprint 56 — commit
`b652822`).

**Étapes de correction :**
1. Avant le cast, vérifier que chaque largeur `w0`/`w1`/`w2` (issue de
   `/W`) est `>= 0` ; si négative, renvoyer une erreur explicite
   (`PdfError::InvalidObject`) au lieu de `unwrap_or(0) as usize`.
2. Utiliser `checked_add` pour `w0+w1+w2` et propager une erreur en cas de
   dépassement plutôt qu'un `+` nu.
3. Ajouter un test avec un `/W` contenant une valeur négative, vérifier
   l'absence de panique (debug et release).

---

## 3. [Mineur] Complexité O(n²) dans `parse_xref_chain` — ✅ Corrigé

**Fichier :** `pdf-core/src/xref.rs:57-84`

**Problème :** `visited: Vec<usize>` avec `visited.contains(&offset)` est
linéaire, appelé à chaque section `/Prev` — un PDF avec de nombreuses
sections xref chaînées entraîne un coût quadratique (dégradation de
performance, pas un crash).

**Étapes de correction :**
1. Remplacer `Vec<usize>` par `HashSet<usize>` pour `visited`.
2. Vérifier que les tests existants sur les chaînes `/Prev`
   (`incremental_updates_chain.pdf`) passent toujours.

---

## 4. [Mineur] Littéraux `f32` non explicites — future-incompatibility — ✅ Corrigé

**Fichier :** `pdf-ui/src/main.rs` (lignes ~3006, 3054, 3083, 3114, 3138)

**Problème :** `cargo clippy` avertit que des littéraux comme `2.5`/`1.0`
utilisés là où un `f32` est attendu (`egui::Stroke::new`, largeurs de
trait) reposent sur une conversion `f32: From<f64>` qui sera rejetée par une
future version de Rust (voir issue rust-lang/rust#154024).

**Étapes de correction :**
1. Suffixer chaque littéral concerné avec `_f32` (ex: `2.5_f32`, `1.0_f32`).
2. Relancer `cargo clippy --workspace --all-targets` pour confirmer la
   disparition des 6 avertissements.

---

## 5. [Info, hors contrôle direct] `block v0.1.6` future-incompatible

Dépendance transitive de l'écosystème `objc2`/AppKit. Pas d'action
immédiate possible côté ce projet — surveiller une mise à jour amont
d'`objc2`/`objc2-app-kit` qui la remplace.

---

## Ordre de traitement recommandé

1. **#1** (récursion infinie) — priorité immédiate, faille de sécurité
   réelle et facilement exploitable (déni de service).
2. **#2** (cast xref) — même famille de bug que des crashs déjà corrigés,
   rapide à corriger.
3. **#3** (perf O(n²)) — rapide, faible risque.
4. **#4** (littéraux f32) — cosmétique mais évite une casse de build future.
5. **#5** — surveillance passive, pas d'action de code.
