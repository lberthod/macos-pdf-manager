# Fuzzing (Sprint 56, `sprint.md` Sprint 23+)

`cargo-fuzz` (libFuzzer) sur `pdf-core` — la surface d'attaque la plus
exposée du projet, puisque n'importe quel fichier ouvert par l'utilisateur y
passe. `corpus/`/`artifacts/`/`target/` sont volontairement ignorés par git
(`.gitignore`) : un corpus exploré par libFuzzer grossit vite (dizaines de
Mo, majoritairement des variations binaires sans valeur documentaire) — voir
plus bas pour le reconstituer depuis le corpus de fixtures déjà versionné.

## Prérequis

`cargo-fuzz` nécessite un toolchain **nightly** (support de
`-Zsanitizer=address`, pas disponible sur stable) :

```sh
cargo install cargo-fuzz
rustup toolchain install nightly --profile minimal
```

## Cibles

- **`parse_document`** — `pdf_core::Document::open` sur des octets
  arbitraires, puis un balayage superficiel (nombre de pages, contenu de
  chaque page, table des matières). Couvre lexer/xref/parser/filtres/
  déchiffrement, sans le coût du rendu.
- **`render_document`** — comme `parse_document`, mais va jusqu'au rendu CPU
  (`pdf-render`) de la première page : couvre en plus l'interpréteur de
  contenu, la résolution de polices et le décodage d'image. Plus lent, donc
  limité à la première page.

Aucune des deux cibles n'échoue sur un `Result::Err` — un PDF malformé doit
renvoyer une erreur propre, pas paniquer ; c'est la panique/le crash que
`cargo fuzz` détecte (`panic = "abort"` implicite sous libFuzzer).

## Lancer

```sh
# Seed initial depuis le corpus de fixtures déjà versionné (pdf-core/tests/fixtures) :
mkdir -p corpus/parse_document corpus/render_document
cp ../pdf-core/tests/fixtures/*.pdf corpus/parse_document/
cp ../pdf-core/tests/fixtures/*.pdf corpus/render_document/

cargo +nightly fuzz run parse_document -- -max_total_time=300
cargo +nightly fuzz run render_document -- -max_total_time=300
```

Un plantage écrit l'entrée dans `artifacts/<cible>/crash-<hash>` et affiche
la commande pour le reproduire/minimiser (`cargo fuzz tmin`).

## Bugs trouvés et corrigés (Sprint 56)

Les deux premières minutes de fuzzing (avant tout durcissement dédié) ont
trouvé deux plantages réels, tous deux des débordements arithmétiques
détectés par les assertions de débordement (actives sous `cargo fuzz`, et
déjà sous n'importe quel build `dev` standard de ce workspace) :

- **`pdf-core::filters::ascii85_decode`** — un octet hors de la plage
  ASCII85 valide (`'!'..='u'`, 33..=117) qui n'était ni espace, ni `~`, ni
  `z` faisait paniquer `b - 33` (`attempt to subtract with overflow`).
  Corrigé : renvoie une erreur propre, comme `ascii_hex_decode` le fait déjà
  pour un chiffre hexadécimal invalide. Régression :
  `pdf-core/src/filters.rs::ascii85_decode_rejects_byte_below_valid_range_instead_of_panicking`.
- **`pdf-core::parser::parse_stream_body`** — un `/Length` négatif faisait
  déborder `*n as usize` (enroulé vers un nombre énorme) puis
  `pos + len` (`attempt to add with overflow`). Corrigé : un `/Length`
  négatif est traité comme absent (repli sur la recherche littérale de
  `endstream`, même chemin que pour une référence indirecte non résolue) ;
  `pos.saturating_add(len)` en défense en profondeur pour le cas positif
  mais absurdement grand. Régression :
  `pdf-core/src/parser.rs::negative_length_falls_back_to_endstream_search_instead_of_panicking`.

Aucun autre plantage trouvé sur ~260 000 exécutions cumulées après ces deux
correctifs (voir sprint.md Sprint 56 pour le détail).

## Non fait

- **CI continue** : pas de job dédié qui lance `cargo fuzz` en continu
  (ex. OSS-Fuzz) — cette passe est une session ponctuelle, pas une
  intégration récurrente.
- **Cible dédiée à l'édition** (`pdf-edit`) : seule la lecture (`pdf-core`)
  est fuzzée pour l'instant — l'édition part toujours d'un document déjà
  ouvert avec succès, surface d'attaque nettement plus restreinte.
