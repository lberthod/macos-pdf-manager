//! Métriques et encodage des polices simples (Type1/TrueType 1 octet) et
//! composites (`/Type0`/CID, codes 2 octets) — architecture.md §4.6. Fournit
//! à l'interpréteur de contenu (`interp.rs`) de quoi remplacer l'avance de
//! texte placeholder par une largeur réelle, et le code de caractère brut
//! par un caractère Unicode.
//!
//! Limitations connues (voir sprint.md, Sprint 7-8+) :
//! - Polices composites (`/Type0`) : gérées (voir `Font::decode_composite`/
//!   `cid_metrics`/`cid_glyph_outline`) mais avec un périmètre volontairement
//!   restreint à l'`/Encoding` **`Identity-H`/`Identity-V`** — code source 2
//!   octets = CID directement, sans indirection par CMap. C'est le cas
//!   couvrant l'immense majorité des PDF `/Type0` réels (LibreOffice, export
//!   PDF de traitements de texte, XeLaTeX...), qui embarquent systématiquement
//!   leur sous-ensemble de police avec cette correspondance triviale. Un
//!   `/Encoding` nommé différent (CMap prédéfini CJK comme `UniGB-UCS2-H`) ou
//!   un flux de CMap embarqué (plages de code de largeur variable,
//!   `usecmap`) sont traités **comme si** c'était `Identity-H` (code brut
//!   utilisé tel quel comme CID) plutôt que rejetés : approximation
//!   généralement fausse dans ce cas précis, mais qui n'empêche pas de
//!   tenter un rendu plutôt que de retomber sur le placeholder complet.
//!   `/CIDFontType2` (TrueType) : contour résolu via `/CIDToGIDMap`
//!   (`/Identity` ou flux explicite CID->GID) puis la table `glyf`.
//!   `/CIDFontType0` (CFF CID-keyed) : `/CIDToGIDMap` ne s'applique pas
//!   (ISO 32000-1 §9.7.4.2) — CID->GID résolu via le charset interne de la
//!   table CFF elle-même (`ttf_parser::cff::Table::glyph_cid`, inversé une
//!   fois au chargement).
//! - `/ToUnicode` (CMap dédié, ISO 32000-1 §9.10.3) : lu et prioritaire sur
//!   l'encodage de base (`base_encoding_by_name`/`/Differences`) quand
//!   présent — voir `parse_to_unicode_cmap` (polices simples, table
//!   256 entrées) et `parse_to_unicode_cmap_wide` (polices composites,
//!   `HashMap` non bornée à 256, mêmes blocs `beginbfchar`/`beginbfrange`).
//!   Les valeurs de destination multi-unités UTF-16 (paires de substitution,
//!   plusieurs caractères) ne sont pas incrémentées dans une plage
//!   `bfrange` : toute la plage reçoit alors la même valeur (approximation
//!   rare en pratique).
//! - Largeurs des polices standard non intégrées : seule Helvetica dispose
//!   d'une table AFM complète (`HELVETICA_WIDTHS`) ; les 13 autres polices
//!   standard retombent sur `DEFAULT_WIDTH`.
//! - `MacRomanEncoding` est approximé par `WinAnsiEncoding` (tables réelles
//!   distinctes au-delà de l'ASCII) faute de table dédiée pour l'instant.
//! - Contours de glyphes : polices **TrueType intégrées** (`/FontFile2`)
//!   et **CFF/Type1C intégrées** (`/FontFile3`, sous-types `Type1C` — CFF
//!   brut — et `OpenType` — conteneur OpenType complet, traité comme du
//!   TrueType puisque `ttf-parser` gère les deux formats de façon unifiée)
//!   via `ttf-parser`, plus **substitution système macOS** pour les polices
//!   standard non intégrées (Helvetica/Times/Courier + alias Arial,
//!   sélection de face gras/italique dans les `.ttc`, voir `system_font`).
//!   Type1 (`/FontFile`, format historique pré-CFF) n'a pas de parseur de
//!   contours. La substitution lit directement les fichiers de
//!   `/System/Library/Fonts` (chemins macOS codés en dur) — un portage
//!   passerait par Core Text ou fontconfig.

use std::collections::HashMap;

use crate::display::PathSegment;
use crate::document::Document;
use crate::encoding::{glyph_name_to_unicode, STANDARD_ENCODING, WIN_ANSI_ENCODING};
use crate::error::Result;
use crate::filters::decode_stream;
use crate::lexer::{Lexer, Token};
use crate::object::{Dictionary, Object};

/// Avance de repli ultime (en millièmes d'em) quand ni `/Widths` ni la table
/// Helvetica ne couvrent un code donné.
const DEFAULT_WIDTH: f64 = 500.0;

/// Valeur par défaut de `/DW` (ISO 32000-1 §9.7.4.3, Table 117) quand le
/// dictionnaire CIDFont ne la précise pas.
const DEFAULT_CID_WIDTH: f64 = 1000.0;

/// `/CIDToGIDMap` (ISO 32000-1 §9.7.4.2) : uniquement pertinent pour un
/// descendant `/CIDFontType2` (TrueType) — `/CIDFontType0` (CFF CID-keyed)
/// résout CID -> GID via le charset interne de la table CFF (voir
/// `CidGlyphSource::Cff`), cette table n'a alors aucun rôle.
enum CidToGid {
    /// CID == GID (valeur par défaut, `/CIDToGIDMap /Identity` ou absent).
    Identity,
    /// Flux explicite : `map[cid]` donne le GID, ISO 32000-1 §9.7.4.2.
    Map(Vec<u16>),
}

/// Source de résolution des contours pour une police composite — diffère
/// selon le sous-type du descendant (`/CIDFontType0` vs `/CIDFontType2`,
/// voir la doc de module).
enum CidGlyphSource {
    TrueType {
        cid_to_gid: CidToGid,
    },
    /// CID -> GID précalculé une fois au chargement en parcourant tous les
    /// glyphes de la table CFF (`ttf_parser::cff::Table::glyph_cid`, qui ne
    /// donne que le sens GID -> CID) — pas de coût par glyphe affiché.
    Cff {
        cid_to_gid: HashMap<u32, u16>,
    },
}

/// Données propres à une police composite (`/Type0`), résolues une fois au
/// chargement à partir du dictionnaire `/DescendantFonts` (voir la doc de
/// module pour le périmètre exact).
struct CidFontData {
    /// CID -> Unicode, depuis `/ToUnicode` (voir `parse_to_unicode_cmap_wide`).
    to_unicode: HashMap<u32, char>,
    /// CID -> largeur (1/1000 em), depuis `/W`.
    widths: HashMap<u32, f64>,
    /// `/DW`, `DEFAULT_CID_WIDTH` si absent.
    default_width: f64,
    /// `None` si le descendant n'a ni `/FontFile2` ni `/FontFile3`
    /// exploitable (aucun contour résolvable, mais Unicode/largeur restent
    /// disponibles).
    glyph_source: Option<CidGlyphSource>,
}

pub struct Font {
    subtype: String,
    encoding: [Option<char>; 256],
    widths: [Option<f64>; 256],
    /// Table de largeurs AFM à utiliser en repli si `/Widths` est absent
    /// (cas normal des 14 polices standard), choisie d'après `/BaseFont`
    /// (voir `standard_width_fallback`). `None` pour une police composite.
    width_fallback: Option<fn(u8) -> Option<f64>>,
    /// Octets bruts d'un `/FontFile2` (TrueType) déjà décodé, s'il y en a un.
    /// Pour une police composite, provient du `/FontDescriptor` du
    /// descendant (`/DescendantFonts`), pas du dictionnaire `/Type0`
    /// lui-même (qui n'en porte pas, ISO 32000-1 §9.7.3).
    embedded_truetype: Option<Vec<u8>>,
    /// Octets bruts d'un `/FontFile3` `/Subtype /Type1C` ou `CIDFontType0C`
    /// (CFF brut, sans conteneur OpenType) déjà décodé, s'il y en a un.
    embedded_cff: Option<Vec<u8>>,
    /// Police système de substitution (octets du fichier partagés via cache
    /// global + index de face dans la collection `.ttc`), pour les polices
    /// standard non intégrées. `None` si la police est intégrée ou si aucune
    /// substitution n'a été trouvée. Toujours `None` pour une police
    /// composite (pas de notion de "police standard" pour `/Type0`).
    system_fallback: Option<(std::sync::Arc<Vec<u8>>, u32)>,
    /// `Some` seulement si `subtype == "Type0"` (voir `is_composite`).
    cid: Option<CidFontData>,
}

impl Font {
    /// Construit une police à partir de son dictionnaire `/Font` déjà résolu.
    pub fn load(doc: &Document, dict: &Dictionary) -> Result<Font> {
        let subtype = dict
            .get("Subtype")
            .and_then(|o| o.as_name())
            .unwrap_or("")
            .to_string();

        let mut encoding = WIN_ANSI_ENCODING;
        let mut differences: Vec<(i64, String)> = Vec::new();

        if let Some(enc_obj) = dict.get("Encoding") {
            let enc = doc.get(enc_obj)?;
            match &enc {
                Object::Name(name) => {
                    encoding = base_encoding_by_name(name).unwrap_or(STANDARD_ENCODING);
                }
                Object::Dictionary(enc_dict) => {
                    if let Some(base_name) = enc_dict.get("BaseEncoding").and_then(|o| o.as_name())
                    {
                        encoding = base_encoding_by_name(base_name).unwrap_or(WIN_ANSI_ENCODING);
                    }
                    if let Some(Object::Array(items)) = enc_dict.get("Differences") {
                        let mut current_code = 0i64;
                        for item in items {
                            match item {
                                Object::Integer(n) => current_code = *n,
                                Object::Name(name) => {
                                    differences.push((current_code, name.clone()));
                                    current_code += 1;
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (code, name) in differences {
            if (0..256).contains(&code) {
                if let Some(ch) = glyph_name_to_unicode(&name) {
                    encoding[code as usize] = Some(ch);
                }
            }
        }

        // `/ToUnicode` prime sur `/Encoding`+`/Differences` pour la
        // résolution Unicode (ISO 32000-1 §9.10.3) : c'est la source
        // explicitement destinée à l'extraction de texte, alors que
        // `/Encoding` sert surtout à la sélection de glyphe.
        if let Some(to_unicode_obj) = dict.get("ToUnicode") {
            if let Ok(Object::Stream(stream)) = doc.get(to_unicode_obj) {
                if let Ok(bytes) = decode_stream(&stream) {
                    for (code, ch) in parse_to_unicode_cmap(&bytes).into_iter().enumerate() {
                        if let Some(ch) = ch {
                            encoding[code] = Some(ch);
                        }
                    }
                }
            }
        }

        let mut widths: [Option<f64>; 256] = [None; 256];
        let first_char = dict.get("FirstChar").and_then(|o| o.as_int());
        let widths_obj = dict.get("Widths").map(|o| doc.get(o)).transpose()?;
        if let (Some(first_char), Some(items)) =
            (first_char, widths_obj.as_ref().and_then(|o| o.as_array()))
        {
            for (i, w) in items.iter().enumerate() {
                let code = first_char + i as i64;
                if (0..256).contains(&code) {
                    if let Some(width) = number(w) {
                        widths[code as usize] = Some(width);
                    }
                }
            }
        }

        let base_font = dict
            .get("BaseFont")
            .and_then(|o| o.as_name())
            .unwrap_or("Helvetica");

        let width_fallback = if subtype != "Type0" && widths.iter().all(|w| w.is_none()) {
            Some(standard_width_fallback(base_font))
        } else {
            None
        };

        let descriptor_dict = dict
            .get("FontDescriptor")
            .and_then(|o| doc.get(o).ok())
            .and_then(|desc| desc.as_dict().cloned());

        let (mut embedded_truetype, mut embedded_cff) =
            load_embedded_font_files(doc, descriptor_dict.as_ref());

        let system_fallback =
            if embedded_truetype.is_none() && embedded_cff.is_none() && subtype != "Type0" {
                system_font::lookup(base_font)
            } else {
                None
            };

        // `/Type0` n'a pas de `/FontDescriptor`/`/Widths` propres (ISO
        // 32000-1 §9.7.3) : tout est porté par son unique descendant
        // (`/DescendantFonts`, un `/CIDFontType0` ou `/CIDFontType2`) — voir
        // la doc de module pour le périmètre (Identity-H/V uniquement).
        let cid = if subtype == "Type0" {
            let descendant_dict = dict
                .get("DescendantFonts")
                .and_then(|o| doc.get(o).ok())
                .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                .and_then(|arr| arr.into_iter().next())
                .and_then(|obj| doc.get(&obj).ok())
                .and_then(|obj| obj.as_dict().cloned());

            let mut to_unicode = HashMap::new();
            if let Some(to_unicode_obj) = dict.get("ToUnicode") {
                if let Ok(Object::Stream(stream)) = doc.get(to_unicode_obj) {
                    if let Ok(bytes) = decode_stream(&stream) {
                        to_unicode = parse_to_unicode_cmap_wide(&bytes);
                    }
                }
            }

            let mut cid_widths = HashMap::new();
            let mut default_width = DEFAULT_CID_WIDTH;
            let mut cid_to_gid = CidToGid::Identity;

            if let Some(cid_dict) = &descendant_dict {
                if let Some(dw) = cid_dict.get("DW").and_then(number) {
                    default_width = dw;
                }
                if let Some(items) = cid_dict
                    .get("W")
                    .and_then(|o| doc.get(o).ok())
                    .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                {
                    parse_cid_widths(&items, &mut cid_widths);
                }

                cid_to_gid = match cid_dict.get("CIDToGIDMap").and_then(|o| doc.get(o).ok()) {
                    Some(Object::Stream(stream)) => decode_stream(&stream)
                        .map(|bytes| {
                            CidToGid::Map(
                                bytes
                                    .chunks_exact(2)
                                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                    .collect(),
                            )
                        })
                        .unwrap_or(CidToGid::Identity),
                    _ => CidToGid::Identity, // `/Identity` (ou absent, valeur par défaut).
                };

                let cid_descriptor_dict = cid_dict
                    .get("FontDescriptor")
                    .and_then(|o| doc.get(o).ok())
                    .and_then(|desc| desc.as_dict().cloned());
                let (descendant_truetype, descendant_cff) =
                    load_embedded_font_files(doc, cid_descriptor_dict.as_ref());
                embedded_truetype = descendant_truetype;
                embedded_cff = descendant_cff;
            }

            let glyph_source = if embedded_truetype.is_some() {
                Some(CidGlyphSource::TrueType { cid_to_gid })
            } else if let Some(cff_bytes) = embedded_cff.as_ref() {
                ttf_parser::cff::Table::parse(cff_bytes).map(|table| {
                    let mut map = HashMap::new();
                    for gid in 0..table.number_of_glyphs() {
                        if let Some(cid) = table.glyph_cid(ttf_parser::GlyphId(gid)) {
                            map.insert(cid as u32, gid);
                        }
                    }
                    CidGlyphSource::Cff { cid_to_gid: map }
                })
            } else {
                None
            };

            Some(CidFontData {
                to_unicode,
                widths: cid_widths,
                default_width,
                glyph_source,
            })
        } else {
            None
        };

        Ok(Font {
            subtype,
            encoding,
            widths,
            width_fallback,
            embedded_truetype,
            embedded_cff,
            system_fallback,
            cid,
        })
    }

    /// Décode un code de caractère 1 octet (polices simples uniquement) en
    /// `(unicode résolu, largeur en 1/1000 em)`.
    pub fn decode_simple(&self, code: u8) -> (Option<char>, f64) {
        let unicode = self.encoding[code as usize];
        let width = self.widths[code as usize]
            .or_else(|| self.width_fallback.and_then(|f| f(code)))
            .unwrap_or(DEFAULT_WIDTH);
        (unicode, width)
    }

    pub fn is_composite(&self) -> bool {
        self.subtype == "Type0"
    }

    /// Contour vectoriel du glyphe correspondant à un code de caractère 1
    /// octet, dans l'espace em normalisé (1.0 = une taille de police), déjà
    /// orienté correctement (Y vers le haut, comme le reste du pipeline
    /// PDF). `None` si aucune police (intégrée ou substituée) n'est
    /// disponible ou si le glyphe est introuvable.
    ///
    /// Résolution du glyphe, TrueType/OpenType : essaie d'abord une table
    /// `cmap` Unicode (`unicode`, si connu) ; sinon, retombe sur une
    /// éventuelle table `cmap` Macintosh (plate-forme 1) interrogée
    /// directement avec le **code brut** — cas fréquent des sous-ensembles
    /// produits par des outils comme reportlab, qui n'embarquent qu'un cmap
    /// Mac Roman indexé par code plutôt que par point de code Unicode.
    /// Résolution CFF : l'encodage/charset intégré à la table CFF elle-même
    /// est interrogé directement par code brut (`cff::Table::glyph_index`),
    /// pas de notion de cmap Unicode dans ce format.
    pub fn glyph_outline(&self, code: u8, unicode: Option<char>) -> Option<Vec<PathSegment>> {
        if let Some(data) = self.embedded_truetype.as_ref() {
            let face = ttf_parser::Face::parse(data, 0).ok()?;
            let gid = unicode.and_then(|u| face.glyph_index(u)).or_else(|| {
                face.tables().cmap.and_then(|cmap| {
                    cmap.subtables
                        .into_iter()
                        .find(|sub| sub.platform_id == ttf_parser::PlatformId::Macintosh)
                        .and_then(|sub| sub.glyph_index(code as u32))
                })
            })?;
            return outline_from_face(&face, gid);
        }

        if let Some(data) = self.embedded_cff.as_ref() {
            let table = ttf_parser::cff::Table::parse(data)?;
            let gid = table.glyph_index(code)?;
            // `sx` de la FontMatrix CFF (0.001 dans l'immense majorité des
            // fontes) : on ignore kx/ky/tx/ty, quasi toujours nuls en pratique.
            let scale = table.matrix().sx as f64;
            let mut collector = OutlineCollector {
                segments: Vec::new(),
                current: (0.0, 0.0),
                scale: if scale != 0.0 { scale } else { 1.0 / 1000.0 },
            };
            table.outline(gid, &mut collector).ok()?;
            return Some(collector.segments);
        }

        // Substitution système (police standard non intégrée) : les polices
        // système ont toujours un cmap Unicode, donc seule la résolution par
        // caractère Unicode a du sens ici.
        let (data, face_index) = self.system_fallback.as_ref()?;
        let face = ttf_parser::Face::parse(data, *face_index).ok()?;
        let gid = face.glyph_index(unicode?)?;
        outline_from_face(&face, gid)
    }

    /// Découpe une chaîne d'une police composite en CIDs (voir la doc de
    /// module : `Identity-H`/`Identity-V` uniquement, code source 2 octets =
    /// CID directement). Un dernier octet isolé (chaîne de longueur impaire,
    /// PDF malformé) est silencieusement ignoré plutôt que de paniquer.
    pub fn decode_composite(&self, bytes: &[u8]) -> Vec<u32> {
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]) as u32)
            .collect()
    }

    /// Décode un CID (police composite) en `(unicode résolu, largeur en
    /// 1/1000 em)` — symétrique de `decode_simple`. Largeur : `/W` puis
    /// `/DW` (`DEFAULT_CID_WIDTH` si la police n'a pas pu être lue comme
    /// composite du tout, ce qui ne devrait pas arriver si `is_composite()`
    /// a été vérifié avant l'appel).
    pub fn cid_metrics(&self, cid: u32) -> (Option<char>, f64) {
        let Some(cid_data) = self.cid.as_ref() else {
            return (None, DEFAULT_CID_WIDTH);
        };
        let unicode = cid_data.to_unicode.get(&cid).copied();
        let width = cid_data
            .widths
            .get(&cid)
            .copied()
            .unwrap_or(cid_data.default_width);
        (unicode, width)
    }

    /// Contour vectoriel du glyphe correspondant à un CID (police
    /// composite), même espace/orientation que `glyph_outline`. `None` si
    /// la police n'a pas de descendant exploitable (`FontFile2`/`FontFile3`
    /// absent ou non supporté) ou si ce CID précis n'a pas de glyphe — voir
    /// `CidGlyphSource` pour la résolution CID -> GID selon le sous-type du
    /// descendant.
    pub fn cid_glyph_outline(&self, cid: u32) -> Option<Vec<PathSegment>> {
        match self.cid.as_ref()?.glyph_source.as_ref()? {
            CidGlyphSource::TrueType { cid_to_gid } => {
                let gid = match cid_to_gid {
                    CidToGid::Identity => u16::try_from(cid).ok()?,
                    CidToGid::Map(map) => *map.get(cid as usize)?,
                };
                let data = self.embedded_truetype.as_ref()?;
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                outline_from_face(&face, ttf_parser::GlyphId(gid))
            }
            CidGlyphSource::Cff { cid_to_gid } => {
                let gid = *cid_to_gid.get(&cid)?;
                let data = self.embedded_cff.as_ref()?;
                let table = ttf_parser::cff::Table::parse(data)?;
                let scale = table.matrix().sx as f64;
                let mut collector = OutlineCollector {
                    segments: Vec::new(),
                    current: (0.0, 0.0),
                    scale: if scale != 0.0 { scale } else { 1.0 / 1000.0 },
                };
                table
                    .outline(ttf_parser::GlyphId(gid), &mut collector)
                    .ok()?;
                Some(collector.segments)
            }
        }
    }
}

/// Charge `/FontFile2` (TrueType) ou `/FontFile3` (CFF/OpenType) depuis un
/// `/FontDescriptor` déjà résolu — factorisé entre les polices simples
/// (`Font::load`, descripteur du dictionnaire `/Font` lui-même) et les
/// polices composites (même logique, mais sur le `/FontDescriptor` du
/// descendant `/DescendantFonts`, qui seul en porte un pour `/Type0`).
fn load_embedded_font_files(
    doc: &Document,
    descriptor_dict: Option<&Dictionary>,
) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut embedded_truetype = descriptor_dict
        .and_then(|d| d.get("FontFile2").cloned())
        .and_then(|obj| doc.get(&obj).ok())
        .and_then(|obj| match obj {
            Object::Stream(stream) => decode_stream(&stream).ok(),
            _ => None,
        });

    let mut embedded_cff = None;
    if embedded_truetype.is_none() {
        if let Some(Object::Stream(stream)) = descriptor_dict
            .and_then(|d| d.get("FontFile3").cloned())
            .and_then(|obj| doc.get(&obj).ok())
        {
            let file_subtype = stream.dict.get("Subtype").and_then(|o| o.as_name());
            if let Ok(bytes) = decode_stream(&stream) {
                match file_subtype {
                    // Conteneur OpenType complet : ttf-parser::Face gère
                    // indifféremment glyf et CFF en interne.
                    Some("OpenType") => embedded_truetype = Some(bytes),
                    // CFF brut (`Type1C`/`CIDFontType0C`) : pas de table
                    // sfnt, nécessite le parseur CFF dédié.
                    _ => embedded_cff = Some(bytes),
                }
            }
        }
    }

    (embedded_truetype, embedded_cff)
}

/// Parse `/W` (ISO 32000-1 §9.7.4.3, Table 117) : deux formes interleavées
/// dans le même tableau — `c [w1 w2 ...]` (largeurs individuelles pour des
/// CIDs consécutifs à partir de `c`) et `cFirst cLast w` (largeur uniforme
/// sur une plage). Entrées malformées (élément inattendu) : le reste du
/// tableau est simplement ignoré plutôt que de paniquer.
fn parse_cid_widths(items: &[Object], out: &mut HashMap<u32, f64>) {
    let mut i = 0;
    while i < items.len() {
        let Some(first) = items.get(i).and_then(number) else {
            return;
        };
        i += 1;
        match items.get(i) {
            Some(Object::Array(run)) => {
                for (offset, w) in run.iter().enumerate() {
                    if let Some(width) = number(w) {
                        out.insert(first as u32 + offset as u32, width);
                    }
                }
                i += 1;
            }
            Some(second) => {
                let Some(last) = number(second) else {
                    return;
                };
                i += 1;
                let Some(width) = items.get(i).and_then(number) else {
                    return;
                };
                i += 1;
                let mut cid = first as i64;
                while cid <= last as i64 {
                    out.insert(cid as u32, width);
                    cid += 1;
                }
            }
            None => return,
        }
    }
}

fn outline_from_face(
    face: &ttf_parser::Face,
    gid: ttf_parser::GlyphId,
) -> Option<Vec<PathSegment>> {
    let units_per_em = face.units_per_em() as f64;
    if units_per_em <= 0.0 {
        return None;
    }
    let mut collector = OutlineCollector {
        segments: Vec::new(),
        current: (0.0, 0.0),
        scale: 1.0 / units_per_em,
    };
    face.outline_glyph(gid, &mut collector)?;
    Some(collector.segments)
}

/// Substitution des 14 polices standard PDF (et alias courants) par les
/// polices système macOS — architecture.md §4.6. Un PDF qui référence
/// `Helvetica` sans l'intégrer suppose que le lecteur la connaît : c'est le
/// cas le plus courant en pratique, et sans cette substitution aucun texte
/// de ces documents ne peut être dessiné.
mod system_font {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};

    /// `None` mémorisé = fichier absent (évite de re-tenter la lecture).
    type FontFileCache = HashMap<&'static str, Option<Arc<Vec<u8>>>>;

    /// Cache global des fichiers de police système (un `.ttc` fait 1-2 Mo ;
    /// on ne veut le lire qu'une fois par processus, pas à chaque `Tj`).
    fn cache() -> &'static Mutex<FontFileCache> {
        static CACHE: OnceLock<Mutex<FontFileCache>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Mappe un `/BaseFont` (préfixe de sous-ensemble déjà toléré) vers un
    /// fichier système + le style attendu (gras, italique).
    pub fn lookup(base_font: &str) -> Option<(Arc<Vec<u8>>, u32)> {
        // Les sous-ensembles sont nommés `ABCDEF+RealName` (ISO 32000-1 §9.6.4).
        let name = base_font.split('+').next_back().unwrap_or(base_font);
        let lower = name.to_ascii_lowercase();

        let path: &'static str = if lower.starts_with("helvetica") || lower.starts_with("arial") {
            "/System/Library/Fonts/Helvetica.ttc"
        } else if lower.starts_with("times") {
            "/System/Library/Fonts/Times.ttc"
        } else if lower.starts_with("courier") {
            "/System/Library/Fonts/Courier.ttc"
        } else if lower.starts_with("symbol") {
            "/System/Library/Fonts/Symbol.ttf"
        } else if lower.starts_with("zapf") {
            "/System/Library/Fonts/ZapfDingbats.ttf"
        } else {
            // Police inconnue non intégrée : Helvetica fait office de repli
            // générique (même choix que la plupart des viewers).
            "/System/Library/Fonts/Helvetica.ttc"
        };

        let bold = lower.contains("bold");
        let italic = lower.contains("italic") || lower.contains("oblique");

        let data = {
            let mut cache = cache().lock().ok()?;
            cache
                .entry(path)
                .or_insert_with(|| std::fs::read(path).ok().map(Arc::new))
                .clone()?
        };

        let face_index = select_face(&data, bold, italic);
        Some((data, face_index))
    }

    /// Choisit la face d'une collection `.ttc` correspondant au style
    /// demandé ; face 0 en repli (fichiers `.ttf` simples inclus).
    fn select_face(data: &[u8], bold: bool, italic: bool) -> u32 {
        let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        for index in 0..count {
            if let Ok(face) = ttf_parser::Face::parse(data, index) {
                if face.is_bold() == bold && face.is_italic() == italic {
                    return index;
                }
            }
        }
        0
    }
}

/// Convertit les commandes de contour `ttf-parser` (unités de police, Y vers
/// le haut comme en PDF) en `PathSegment`, à l'échelle 1 unité = 1 em. Les
/// courbes quadratiques (`quad_to`, format natif TrueType) sont élevées en
/// cubiques pour rester homogènes avec le reste du pipeline (`c`/`v`/`y`
/// PDF ne produisent que des cubiques).
struct OutlineCollector {
    segments: Vec<PathSegment>,
    current: (f32, f32),
    scale: f64,
}

impl OutlineCollector {
    fn pt(&self, x: f32, y: f32) -> (f64, f64) {
        (x as f64 * self.scale, y as f64 * self.scale)
    }
}

impl ttf_parser::OutlineBuilder for OutlineCollector {
    fn move_to(&mut self, x: f32, y: f32) {
        self.segments.push(PathSegment::MoveTo(self.pt(x, y)));
        self.current = (x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.segments.push(PathSegment::LineTo(self.pt(x, y)));
        self.current = (x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // Élévation degré 2 -> 3 : c1 = p0 + 2/3(q-p0), c2 = p2 + 2/3(q-p2).
        let (x0, y0) = self.current;
        let c1x = x0 + 2.0 / 3.0 * (x1 - x0);
        let c1y = y0 + 2.0 / 3.0 * (y1 - y0);
        let c2x = x + 2.0 / 3.0 * (x1 - x);
        let c2y = y + 2.0 / 3.0 * (y1 - y);
        self.segments.push(PathSegment::CurveTo {
            c1: self.pt(c1x, c1y),
            c2: self.pt(c2x, c2y),
            to: self.pt(x, y),
        });
        self.current = (x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.segments.push(PathSegment::CurveTo {
            c1: self.pt(x1, y1),
            c2: self.pt(x2, y2),
            to: self.pt(x, y),
        });
        self.current = (x, y);
    }

    fn close(&mut self) {
        self.segments.push(PathSegment::ClosePath);
    }
}

fn number(obj: &Object) -> Option<f64> {
    match obj {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(f) => Some(*f),
        _ => None,
    }
}

/// Convertit un code source hexadécimal (`<...>`, 1 ou plusieurs octets) en
/// entier, uniquement s'il tient dans `0..256` (seuls les codes 1 octet des
/// polices simples nous intéressent ici).
fn hex_to_code(bytes: &[u8]) -> Option<usize> {
    let value = bytes.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32);
    (value < 256).then_some(value as usize)
}

/// Décode une valeur de destination `/ToUnicode` (UTF-16BE, éventuellement
/// une paire de substitution) en son premier caractère.
fn hex_to_char(bytes: &[u8]) -> Option<char> {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    char::decode_utf16(units).next()?.ok()
}

/// Parse un CMap `/ToUnicode` (ISO 32000-1 §9.10.3) : blocs `beginbfchar`
/// (mappings 1:1) et `beginbfrange` (plages contiguës, destination "base +
/// décalage" ou tableau explicite). Le format est un sous-ensemble de la
/// syntaxe PostScript ; les chaînes hexadécimales (`<...>`) et mots-clés se
/// tokenisent comme de la syntaxe PDF standard, d'où la réutilisation du
/// `Lexer` de `pdf-core` plutôt qu'un parseur dédié.
fn parse_to_unicode_cmap(bytes: &[u8]) -> [Option<char>; 256] {
    let mut map: [Option<char>; 256] = [None; 256];
    let mut lexer = Lexer::new(bytes);

    while let Ok(Some(token)) = lexer.next_token() {
        match token {
            Token::Keyword(kw) if kw == "beginbfchar" => {
                while let Ok(Some(Token::HexString(src))) = lexer.next_token() {
                    // `endbfchar` (ou fin de flux) : fin du bloc.
                    let Ok(Some(Token::HexString(dst))) = lexer.next_token() else {
                        break;
                    };
                    if let (Some(code), Some(ch)) = (hex_to_code(&src), hex_to_char(&dst)) {
                        map[code] = Some(ch);
                    }
                }
            }
            Token::Keyword(kw) if kw == "beginbfrange" => {
                while let Ok(Some(Token::HexString(lo))) = lexer.next_token() {
                    // `endbfrange` (ou fin de flux) : fin du bloc.
                    let Ok(Some(Token::HexString(hi))) = lexer.next_token() else {
                        break;
                    };
                    let Ok(Some(dst_token)) = lexer.next_token() else {
                        break;
                    };
                    let (Some(lo_code), Some(hi_code)) = (hex_to_code(&lo), hex_to_code(&hi))
                    else {
                        continue;
                    };
                    match dst_token {
                        Token::HexString(base) => {
                            let base_units: Vec<u16> = base
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                            if let [unit] = base_units[..] {
                                for (offset, slot) in
                                    map.iter_mut().enumerate().take(hi_code + 1).skip(lo_code)
                                {
                                    let code_point =
                                        (unit as u32).wrapping_add((offset - lo_code) as u32);
                                    if let Some(ch) = char::from_u32(code_point) {
                                        *slot = Some(ch);
                                    }
                                }
                            } else if let Some(ch) =
                                char::decode_utf16(base_units).next().and_then(|r| r.ok())
                            {
                                // Destination multi-unités : pas d'incrément
                                // simple possible, toute la plage reçoit la
                                // même valeur (voir limitation en tête de fichier).
                                for slot in map.iter_mut().take(hi_code + 1).skip(lo_code) {
                                    *slot = Some(ch);
                                }
                            }
                        }
                        Token::ArrayStart => {
                            for slot in map.iter_mut().take(hi_code + 1).skip(lo_code) {
                                match lexer.next_token() {
                                    Ok(Some(Token::HexString(dst))) => {
                                        if let Some(ch) = hex_to_char(&dst) {
                                            *slot = Some(ch);
                                        }
                                    }
                                    _ => break,
                                }
                            }
                            let _ = lexer.next_token(); // consomme le `]`.
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    map
}

/// Comme `hex_to_code`, sans la restriction à `0..256` : un CID (police
/// composite) tient sur 2 octets, potentiellement plus large qu'un code de
/// police simple.
fn hex_to_code_u32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0u32, |acc, b| (acc << 8) | *b as u32)
}

/// Comme `parse_to_unicode_cmap`, pour une police composite : source ->
/// destination non bornée à 256 (`HashMap` plutôt que tableau 256 entrées).
/// Mêmes blocs gérés (`beginbfchar`/`beginbfrange`, même limitation sur les
/// destinations multi-unités dans une plage — voir la doc de module).
fn parse_to_unicode_cmap_wide(bytes: &[u8]) -> HashMap<u32, char> {
    let mut map = HashMap::new();
    let mut lexer = Lexer::new(bytes);

    while let Ok(Some(token)) = lexer.next_token() {
        match token {
            Token::Keyword(kw) if kw == "beginbfchar" => {
                while let Ok(Some(Token::HexString(src))) = lexer.next_token() {
                    let Ok(Some(Token::HexString(dst))) = lexer.next_token() else {
                        break;
                    };
                    if let Some(ch) = hex_to_char(&dst) {
                        map.insert(hex_to_code_u32(&src), ch);
                    }
                }
            }
            Token::Keyword(kw) if kw == "beginbfrange" => {
                while let Ok(Some(Token::HexString(lo))) = lexer.next_token() {
                    let Ok(Some(Token::HexString(hi))) = lexer.next_token() else {
                        break;
                    };
                    let Ok(Some(dst_token)) = lexer.next_token() else {
                        break;
                    };
                    let lo_code = hex_to_code_u32(&lo);
                    let hi_code = hex_to_code_u32(&hi);
                    match dst_token {
                        Token::HexString(base) => {
                            let base_units: Vec<u16> = base
                                .chunks_exact(2)
                                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                                .collect();
                            if let [unit] = base_units[..] {
                                for offset in 0..=(hi_code - lo_code) {
                                    let code_point = (unit as u32).wrapping_add(offset);
                                    if let Some(ch) = char::from_u32(code_point) {
                                        map.insert(lo_code + offset, ch);
                                    }
                                }
                            } else if let Some(ch) =
                                char::decode_utf16(base_units).next().and_then(|r| r.ok())
                            {
                                // Destination multi-unités : toute la plage
                                // reçoit la même valeur (voir limitation en
                                // tête de fichier).
                                for code in lo_code..=hi_code {
                                    map.insert(code, ch);
                                }
                            }
                        }
                        Token::ArrayStart => {
                            for code in lo_code..=hi_code {
                                match lexer.next_token() {
                                    Ok(Some(Token::HexString(dst))) => {
                                        if let Some(ch) = hex_to_char(&dst) {
                                            map.insert(code, ch);
                                        }
                                    }
                                    _ => break,
                                }
                            }
                            let _ = lexer.next_token(); // consomme le `]`.
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    map
}

fn base_encoding_by_name(name: &str) -> Option<[Option<char>; 256]> {
    match name {
        "WinAnsiEncoding" => Some(WIN_ANSI_ENCODING),
        "StandardEncoding" => Some(STANDARD_ENCODING),
        "MacRomanEncoding" => Some(WIN_ANSI_ENCODING), // approximation, voir limitations ci-dessus.
        _ => None,
    }
}

/// Choisit la table de largeurs AFM Core 14 à utiliser en repli d'après le
/// nom de `/BaseFont` (préfixe de sous-ensemble toléré, comme
/// `system_font::lookup`) : Times/Courier ont des métriques très différentes
/// d'Helvetica (largeurs proportionnelles vs Times, ou fixes pour Courier),
/// et les confondre décale les glyphes suivants au sein d'un mot (largeur
/// trop grande ou trop petite qui s'accumule), d'où les espaces parasites
/// visibles au milieu des mots en Times/Courier avant ce correctif. Tout nom
/// non reconnu (Helvetica/Arial, Symbol, ZapfDingbats, ...) retombe sur les
/// largeurs Helvetica, comme avant.
fn standard_width_fallback(base_font: &str) -> fn(u8) -> Option<f64> {
    let name = base_font.split('+').next_back().unwrap_or(base_font);
    let lower = name.to_ascii_lowercase();
    if lower.contains("courier") {
        courier_width
    } else if lower.contains("times") {
        let bold = lower.contains("bold");
        let italic = lower.contains("italic") || lower.contains("oblique");
        match (bold, italic) {
            (true, true) => times_bold_italic_width,
            (true, false) => times_bold_width,
            (false, true) => times_italic_width,
            (false, false) => times_roman_width,
        }
    } else if lower.contains("helvetica") || lower.contains("arial") {
        // Contrairement à Times, l'oblique d'Helvetica **partage** les
        // largeurs du romain dans les métriques Adobe Core 14 (l'oblique
        // n'est qu'un cisaillement géométrique, pas un dessin différent) —
        // seul le gras a de vraies largeurs différentes (glyphes plus
        // larges). Les confondre produit exactement le même symptôme que
        // pour Times/Courier ci-dessus : dérive cumulative de l'espacement
        // au sein des mots en gras, plus visible sur un titre entier en
        // gras (repéré sur un vrai document, largeurs Bold jusque-là
        // confondues avec le Roman).
        if lower.contains("bold") {
            helvetica_bold_width
        } else {
            helvetica_width
        }
    } else {
        helvetica_width
    }
}

/// Largeurs Courier (Adobe Core 14 AFM) : chasse fixe, communes aux 4
/// variantes (Roman/Bold/Italique/BoldItalique).
/// Lettre ASCII de base dont un code `WinAnsiEncoding` accentué (0xC0-0xFF,
/// lettres latines composées — À, é, ï, ñ, ø...) partage la largeur dans
/// les métriques Adobe Core 14 : l'accent ne change pas l'empattement du
/// glyphe de base pour ces polices, seul son dessin diffère. Trouvé en
/// pratique sur un vrai document (français, donc plein d'accents) : les
/// codes au-delà de 126 retombaient sur `DEFAULT_WIDTH` (500), très
/// différent de la vraie largeur (ex. 278 pour "ï", comme "i") — d'où des
/// espaces parasites au milieu de mots comme "Loïc" ("Loï c"). Ligatures et
/// lettres sans équivalent ASCII simple (Æ, Ð, ß, Þ, Œ) n'ont pas de base
/// et retombent sur `winansi_symbol_width`.
fn winansi_accent_base(code: u8) -> Option<u8> {
    Some(match code {
        0xC0..=0xC5 => b'A',
        0xC7 => b'C',
        0xC8..=0xCB => b'E',
        0xCC..=0xCF => b'I',
        0xD1 => b'N',
        0xD2..=0xD6 | 0xD8 => b'O',
        0xD9..=0xDC => b'U',
        0xDD => b'Y',
        0xE0..=0xE5 => b'a',
        0xE7 => b'c',
        0xE8..=0xEB => b'e',
        0xEC..=0xEF => b'i',
        0xF1 => b'n',
        0xF2..=0xF6 | 0xF8 => b'o',
        0xF9..=0xFC => b'u',
        0xFD | 0xFF => b'y',
        _ => return None,
    })
}

/// Largeurs approximatives (métriques Helvetica, appliquées telles quelles
/// aux autres polices standard par simplicité — l'écart entre familles pour
/// ces symboles est faible comparé à l'écart avec `DEFAULT_WIDTH`) pour la
/// ponctuation/les symboles `WinAnsiEncoding` sans lettre de base
/// (tirets cadratin/demi-cadratin, guillemets typographiques, points de
/// suspension, puce, œ/Œ...) — le reste des codes hauts non couverts ici
/// retombe sur `DEFAULT_WIDTH`, un dernier repli plus rare en pratique.
fn winansi_symbol_width(code: u8) -> Option<f64> {
    Some(match code {
        0x80 => 556.0,        // euro
        0x82 => 222.0,        // quotesinglbase
        0x83 => 556.0,        // florin
        0x84 => 333.0,        // quotedblbase
        0x85 => 1000.0,       // ellipsis
        0x86 => 556.0,        // dagger
        0x87 => 556.0,        // daggerdbl
        0x88 => 333.0,        // circumflex
        0x89 => 1000.0,       // perthousand
        0x8B => 333.0,        // guilsinglleft
        0x8C => 1000.0,       // OE
        0x91 | 0x92 => 222.0, // quoteleft/quoteright
        0x93 | 0x94 => 333.0, // quotedblleft/quotedblright
        0x95 => 350.0,        // bullet
        0x96 => 556.0,        // endash
        0x97 => 1000.0,       // emdash
        0x98 => 333.0,        // tilde
        0x99 => 1000.0,       // trademark
        0x9B => 333.0,        // guilsinglright
        0x9C => 944.0,        // oe
        0xA0 => 278.0,        // espace insécable, comme l'espace normale
        0xA1 => 333.0,        // exclamdown
        0xA9 => 760.0,        // copyright
        0xAB => 556.0,        // guillemotleft
        0xAE => 760.0,        // registered
        0xBB => 556.0,        // guillemotright
        0xBF => 611.0,        // questiondown
        0xC6 => 1000.0,       // AE
        0xE6 => 722.0,        // ae
        0xDF => 611.0,        // germandbls
        _ => return None,
    })
}

fn courier_width(code: u8) -> Option<f64> {
    let printable = (32..=126).contains(&code)
        || winansi_accent_base(code).is_some()
        || winansi_symbol_width(code).is_some();
    printable.then_some(600.0)
}

/// Largeurs Times-Roman (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126.
fn times_roman_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 250.0,
        33 => 333.0,
        34 => 408.0,
        35 => 500.0,
        36 => 500.0,
        37 => 833.0,
        38 => 778.0,
        39 => 180.0,
        40 => 333.0,
        41 => 333.0,
        42 => 500.0,
        43 => 564.0,
        44 => 250.0,
        45 => 333.0,
        46 => 250.0,
        47 => 278.0,
        48..=57 => 500.0,
        58 => 278.0,
        59 => 278.0,
        60 => 564.0,
        61 => 564.0,
        62 => 564.0,
        63 => 444.0,
        64 => 921.0,
        65 => 722.0,
        66 => 667.0,
        67 => 667.0,
        68 => 722.0,
        69 => 611.0,
        70 => 556.0,
        71 => 722.0,
        72 => 722.0,
        73 => 333.0,
        74 => 389.0,
        75 => 722.0,
        76 => 611.0,
        77 => 889.0,
        78 => 722.0,
        79 => 722.0,
        80 => 556.0,
        81 => 722.0,
        82 => 667.0,
        83 => 556.0,
        84 => 611.0,
        85 => 722.0,
        86 => 722.0,
        87 => 944.0,
        88 => 722.0,
        89 => 722.0,
        90 => 611.0,
        91 => 333.0,
        92 => 278.0,
        93 => 333.0,
        94 => 469.0,
        95 => 500.0,
        96 => 333.0,
        97 => 444.0,
        98 => 500.0,
        99 => 444.0,
        100 => 500.0,
        101 => 444.0,
        102 => 333.0,
        103 => 500.0,
        104 => 500.0,
        105 => 278.0,
        106 => 278.0,
        107 => 500.0,
        108 => 278.0,
        109 => 778.0,
        110 => 500.0,
        111 => 500.0,
        112 => 500.0,
        113 => 500.0,
        114 => 333.0,
        115 => 389.0,
        116 => 278.0,
        117 => 500.0,
        118 => 500.0,
        119 => 722.0,
        120 => 500.0,
        121 => 500.0,
        122 => 444.0,
        123 => 480.0,
        124 => 200.0,
        125 => 480.0,
        126 => 541.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(times_roman_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

/// Largeurs Times-Bold (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126.
fn times_bold_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 250.0,
        33 => 333.0,
        34 => 555.0,
        35 => 500.0,
        36 => 500.0,
        37 => 1000.0,
        38 => 833.0,
        39 => 278.0,
        40 => 333.0,
        41 => 333.0,
        42 => 500.0,
        43 => 570.0,
        44 => 250.0,
        45 => 333.0,
        46 => 250.0,
        47 => 278.0,
        48..=57 => 500.0,
        58 => 333.0,
        59 => 333.0,
        60 => 570.0,
        61 => 570.0,
        62 => 570.0,
        63 => 500.0,
        64 => 930.0,
        65 => 722.0,
        66 => 667.0,
        67 => 722.0,
        68 => 722.0,
        69 => 667.0,
        70 => 611.0,
        71 => 778.0,
        72 => 778.0,
        73 => 389.0,
        74 => 500.0,
        75 => 778.0,
        76 => 667.0,
        77 => 944.0,
        78 => 722.0,
        79 => 778.0,
        80 => 611.0,
        81 => 778.0,
        82 => 722.0,
        83 => 556.0,
        84 => 667.0,
        85 => 722.0,
        86 => 722.0,
        87 => 1000.0,
        88 => 722.0,
        89 => 722.0,
        90 => 667.0,
        91 => 333.0,
        92 => 278.0,
        93 => 333.0,
        94 => 581.0,
        95 => 500.0,
        96 => 333.0,
        97 => 500.0,
        98 => 556.0,
        99 => 444.0,
        100 => 556.0,
        101 => 444.0,
        102 => 333.0,
        103 => 500.0,
        104 => 556.0,
        105 => 278.0,
        106 => 333.0,
        107 => 556.0,
        108 => 278.0,
        109 => 833.0,
        110 => 556.0,
        111 => 500.0,
        112 => 556.0,
        113 => 556.0,
        114 => 444.0,
        115 => 389.0,
        116 => 333.0,
        117 => 556.0,
        118 => 500.0,
        119 => 722.0,
        120 => 500.0,
        121 => 500.0,
        122 => 444.0,
        123 => 394.0,
        124 => 220.0,
        125 => 394.0,
        126 => 520.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(times_bold_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

/// Largeurs Times-Italic (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126.
fn times_italic_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 250.0,
        33 => 333.0,
        34 => 420.0,
        35 => 500.0,
        36 => 500.0,
        37 => 833.0,
        38 => 778.0,
        39 => 214.0,
        40 => 333.0,
        41 => 333.0,
        42 => 500.0,
        43 => 675.0,
        44 => 250.0,
        45 => 333.0,
        46 => 250.0,
        47 => 278.0,
        48..=57 => 500.0,
        58 => 333.0,
        59 => 333.0,
        60 => 675.0,
        61 => 675.0,
        62 => 675.0,
        63 => 500.0,
        64 => 920.0,
        65 => 611.0,
        66 => 611.0,
        67 => 667.0,
        68 => 722.0,
        69 => 611.0,
        70 => 611.0,
        71 => 722.0,
        72 => 722.0,
        73 => 333.0,
        74 => 444.0,
        75 => 667.0,
        76 => 556.0,
        77 => 833.0,
        78 => 667.0,
        79 => 722.0,
        80 => 611.0,
        81 => 722.0,
        82 => 611.0,
        83 => 500.0,
        84 => 556.0,
        85 => 722.0,
        86 => 611.0,
        87 => 833.0,
        88 => 611.0,
        89 => 556.0,
        90 => 556.0,
        91 => 389.0,
        92 => 278.0,
        93 => 389.0,
        94 => 422.0,
        95 => 500.0,
        96 => 333.0,
        97 => 500.0,
        98 => 500.0,
        99 => 444.0,
        100 => 500.0,
        101 => 444.0,
        102 => 278.0,
        103 => 500.0,
        104 => 500.0,
        105 => 278.0,
        106 => 278.0,
        107 => 444.0,
        108 => 278.0,
        109 => 722.0,
        110 => 500.0,
        111 => 500.0,
        112 => 500.0,
        113 => 500.0,
        114 => 389.0,
        115 => 389.0,
        116 => 278.0,
        117 => 500.0,
        118 => 444.0,
        119 => 667.0,
        120 => 444.0,
        121 => 444.0,
        122 => 389.0,
        123 => 400.0,
        124 => 275.0,
        125 => 400.0,
        126 => 541.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(times_italic_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

/// Largeurs Times-BoldItalic (Adobe Core 14 AFM), millièmes d'em, codes
/// ASCII imprimables 32-126.
fn times_bold_italic_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 250.0,
        33 => 389.0,
        34 => 555.0,
        35 => 500.0,
        36 => 500.0,
        37 => 833.0,
        38 => 778.0,
        39 => 278.0,
        40 => 333.0,
        41 => 333.0,
        42 => 500.0,
        43 => 570.0,
        44 => 250.0,
        45 => 333.0,
        46 => 250.0,
        47 => 278.0,
        48..=57 => 500.0,
        58 => 333.0,
        59 => 333.0,
        60 => 570.0,
        61 => 570.0,
        62 => 570.0,
        63 => 500.0,
        64 => 832.0,
        65 => 667.0,
        66 => 667.0,
        67 => 667.0,
        68 => 722.0,
        69 => 667.0,
        70 => 667.0,
        71 => 722.0,
        72 => 778.0,
        73 => 389.0,
        74 => 500.0,
        75 => 667.0,
        76 => 611.0,
        77 => 889.0,
        78 => 722.0,
        79 => 722.0,
        80 => 611.0,
        81 => 722.0,
        82 => 667.0,
        83 => 556.0,
        84 => 611.0,
        85 => 722.0,
        86 => 667.0,
        87 => 889.0,
        88 => 667.0,
        89 => 611.0,
        90 => 611.0,
        91 => 333.0,
        92 => 278.0,
        93 => 333.0,
        94 => 570.0,
        95 => 500.0,
        96 => 333.0,
        97 => 500.0,
        98 => 500.0,
        99 => 444.0,
        100 => 500.0,
        101 => 444.0,
        102 => 333.0,
        103 => 500.0,
        104 => 556.0,
        105 => 278.0,
        106 => 278.0,
        107 => 500.0,
        108 => 278.0,
        109 => 778.0,
        110 => 556.0,
        111 => 500.0,
        112 => 500.0,
        113 => 500.0,
        114 => 389.0,
        115 => 389.0,
        116 => 278.0,
        117 => 556.0,
        118 => 444.0,
        119 => 667.0,
        120 => 500.0,
        121 => 444.0,
        122 => 389.0,
        123 => 348.0,
        124 => 220.0,
        125 => 348.0,
        126 => 570.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(times_bold_italic_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

/// Largeurs Helvetica (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126. Utilisées en repli quand une police standard non
/// intégrée ne fournit pas de `/Widths` — cas normal pour les 14 polices
/// standard, que le lecteur est censé connaître nativement.
fn helvetica_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 278.0,
        33 => 278.0,
        34 => 355.0,
        35 => 556.0,
        36 => 556.0,
        37 => 889.0,
        38 => 667.0,
        39 => 191.0,
        40 => 333.0,
        41 => 333.0,
        42 => 389.0,
        43 => 584.0,
        44 => 278.0,
        45 => 333.0,
        46 => 278.0,
        47 => 278.0,
        48..=57 => 556.0,
        58 => 278.0,
        59 => 278.0,
        60 => 584.0,
        61 => 584.0,
        62 => 584.0,
        63 => 556.0,
        64 => 1015.0,
        65 => 667.0,
        66 => 667.0,
        67 => 722.0,
        68 => 722.0,
        69 => 667.0,
        70 => 611.0,
        71 => 778.0,
        72 => 722.0,
        73 => 278.0,
        74 => 500.0,
        75 => 667.0,
        76 => 556.0,
        77 => 833.0,
        78 => 722.0,
        79 => 778.0,
        80 => 667.0,
        81 => 778.0,
        82 => 722.0,
        83 => 667.0,
        84 => 611.0,
        85 => 722.0,
        86 => 667.0,
        87 => 944.0,
        88 => 667.0,
        89 => 667.0,
        90 => 611.0,
        91 => 278.0,
        92 => 278.0,
        93 => 278.0,
        94 => 469.0,
        95 => 556.0,
        96 => 333.0,
        97 => 556.0,
        98 => 556.0,
        99 => 500.0,
        100 => 556.0,
        101 => 556.0,
        102 => 278.0,
        103 => 556.0,
        104 => 556.0,
        105 => 222.0,
        106 => 222.0,
        107 => 500.0,
        108 => 222.0,
        109 => 833.0,
        110 => 556.0,
        111 => 556.0,
        112 => 556.0,
        113 => 556.0,
        114 => 333.0,
        115 => 500.0,
        116 => 278.0,
        117 => 556.0,
        118 => 500.0,
        119 => 722.0,
        120 => 500.0,
        121 => 500.0,
        122 => 500.0,
        123 => 334.0,
        124 => 260.0,
        125 => 334.0,
        126 => 584.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(helvetica_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

/// Largeurs Helvetica-Bold (Adobe Core 14 AFM), millièmes d'em, codes ASCII
/// imprimables 32-126 — partagées avec Helvetica-BoldOblique (voir
/// `standard_width_fallback`).
fn helvetica_bold_width(code: u8) -> Option<f64> {
    Some(match code {
        32 => 278.0,
        33 => 333.0,
        34 => 474.0,
        35 => 556.0,
        36 => 556.0,
        37 => 889.0,
        38 => 722.0,
        39 => 238.0,
        40 => 333.0,
        41 => 333.0,
        42 => 389.0,
        43 => 584.0,
        44 => 278.0,
        45 => 333.0,
        46 => 278.0,
        47 => 278.0,
        48..=57 => 556.0,
        58 => 333.0,
        59 => 333.0,
        60 => 584.0,
        61 => 584.0,
        62 => 584.0,
        63 => 611.0,
        64 => 975.0,
        65 => 722.0,
        66 => 722.0,
        67 => 722.0,
        68 => 722.0,
        69 => 667.0,
        70 => 611.0,
        71 => 778.0,
        72 => 722.0,
        73 => 278.0,
        74 => 556.0,
        75 => 722.0,
        76 => 611.0,
        77 => 833.0,
        78 => 722.0,
        79 => 778.0,
        80 => 667.0,
        81 => 778.0,
        82 => 722.0,
        83 => 667.0,
        84 => 611.0,
        85 => 722.0,
        86 => 667.0,
        87 => 944.0,
        88 => 667.0,
        89 => 667.0,
        90 => 611.0,
        91 => 333.0,
        92 => 278.0,
        93 => 333.0,
        94 => 584.0,
        95 => 556.0,
        96 => 333.0,
        97 => 556.0,
        98 => 611.0,
        99 => 556.0,
        100 => 611.0,
        101 => 556.0,
        102 => 333.0,
        103 => 611.0,
        104 => 611.0,
        105 => 278.0,
        106 => 278.0,
        107 => 556.0,
        108 => 278.0,
        109 => 889.0,
        110 => 611.0,
        111 => 611.0,
        112 => 611.0,
        113 => 611.0,
        114 => 389.0,
        115 => 556.0,
        116 => 333.0,
        117 => 611.0,
        118 => 556.0,
        119 => 778.0,
        120 => 556.0,
        121 => 556.0,
        122 => 500.0,
        123 => 389.0,
        124 => 280.0,
        125 => 389.0,
        126 => 584.0,
        _ => {
            return winansi_accent_base(code)
                .and_then(helvetica_bold_width)
                .or_else(|| winansi_symbol_width(code))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font_dict(entries: &[(&str, Object)]) -> Dictionary {
        let mut dict = Dictionary::new();
        for (k, v) in entries {
            dict.insert(*k, v.clone());
        }
        dict
    }

    fn dummy_doc() -> Document {
        // Document minimal juste pour permettre `doc.get()` sur des objets
        // directs (aucune référence indirecte utilisée dans ces tests).
        let body = concat!(
            "%PDF-1.7\n",
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
            "2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n",
        );
        let mut bytes = body.as_bytes().to_vec();
        let off1 = bytes.windows(7).position(|w| w == b"1 0 obj").unwrap();
        let off2 = bytes.windows(7).position(|w| w == b"2 0 obj").unwrap();
        let xref_offset = bytes.len();
        let xref = format!(
            "xref\n0 3\n0000000000 65535 f \n{off1:010} 00000 n \n{off2:010} 00000 n \ntrailer\n<< /Size 3 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF"
        );
        bytes.extend_from_slice(xref.as_bytes());
        Document::open(bytes).unwrap()
    }

    #[test]
    fn helvetica_fallback_widths_without_widths_array() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (unicode, width) = font.decode_simple(b'A');
        assert_eq!(unicode, Some('A'));
        assert_eq!(width, 667.0);
    }

    /// Un vrai document (généré par ReportLab, sans `/Widths` — cas courant
    /// pour les 14 polices standard) mélangeant `/Helvetica` et
    /// `/Helvetica-Bold` sans `/Widths` a mis en évidence que les deux
    /// utilisaient la même table de largeurs (celle du romain), alors que
    /// les glyphes gras réels sont plus larges — la dérive cumulative en
    /// résultant à l'intérieur des mots en gras produisait un espacement
    /// visiblement trop large. `Helvetica-Bold` doit désormais utiliser sa
    /// propre table (`helvetica_bold_width`), distincte de `Helvetica`.
    #[test]
    fn helvetica_bold_fallback_uses_wider_bold_metrics_not_roman() {
        let doc = dummy_doc();
        let regular = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let bold = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica-Bold".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let regular_font = Font::load(&doc, &regular).unwrap();
        let bold_font = Font::load(&doc, &bold).unwrap();

        let (_, regular_width) = regular_font.decode_simple(b'A');
        let (_, bold_width) = bold_font.decode_simple(b'A');
        assert_eq!(regular_width, 667.0);
        assert_eq!(
            bold_width, 722.0,
            "Helvetica-Bold 'A' should use its own (wider) AFM width"
        );
        assert_ne!(
            regular_width, bold_width,
            "bold and roman must not silently share the same width table"
        );
    }

    /// Trouvé sur un vrai document français (beaucoup d'accents et de
    /// tirets cadratin) : les codes `WinAnsiEncoding` au-delà de 126
    /// (lettres accentuées, ponctuation typographique) retombaient sur
    /// `DEFAULT_WIDTH` (500), très différent de la vraie largeur — d'où des
    /// espaces parasites (ex. "Loïc" rendu "Loï c") ou des mots collés
    /// (après un tiret cadratin, dont la vraie largeur est 1000, pas 500).
    #[test]
    fn helvetica_fallback_resolves_accented_and_punctuation_codes_correctly() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let font = Font::load(&doc, &dict).unwrap();

        // "ï" (idieresis, 0xEF) doit partager la largeur de "i" (222), pas
        // retomber sur le placeholder 500.
        let (unicode, width) = font.decode_simple(0xEF);
        assert_eq!(unicode, Some('\u{00EF}'));
        assert_eq!(width, 222.0);

        // "é" (eacute, 0xE9) doit partager la largeur de "e" (556).
        let (_, width) = font.decode_simple(0xE9);
        assert_eq!(width, 556.0);

        // Tiret cadratin ("—", emdash, 0x97) : vraie largeur 1000, pas le
        // placeholder 500 (qui collerait le mot suivant contre le tiret).
        let (unicode, width) = font.decode_simple(0x97);
        assert_eq!(unicode, Some('\u{2014}'));
        assert_eq!(width, 1000.0);
    }

    #[test]
    fn explicit_widths_array_take_priority() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("TrueType".into())),
            ("FirstChar", Object::Integer(65)),
            (
                "Widths",
                Object::Array(vec![Object::Integer(999), Object::Integer(999)]),
            ),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (_, width) = font.decode_simple(b'A');
        assert_eq!(width, 999.0);
    }

    #[test]
    fn differences_override_base_encoding() {
        let doc = dummy_doc();
        let mut enc_dict = Dictionary::new();
        enc_dict.insert("BaseEncoding", Object::Name("WinAnsiEncoding".into()));
        enc_dict.insert(
            "Differences",
            Object::Array(vec![Object::Integer(65), Object::Name("eacute".into())]),
        );
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("Encoding", Object::Dictionary(enc_dict)),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let (unicode, _) = font.decode_simple(65);
        assert_eq!(unicode, Some('\u{00E9}'));
    }

    #[test]
    fn type0_is_reported_composite() {
        let doc = dummy_doc();
        let dict = font_dict(&[("Subtype", Object::Name("Type0".into()))]);
        let font = Font::load(&doc, &dict).unwrap();
        assert!(font.is_composite());
    }

    #[test]
    fn decode_composite_splits_two_byte_codes_and_drops_a_trailing_odd_byte() {
        let doc = dummy_doc();
        let dict = font_dict(&[("Subtype", Object::Name("Type0".into()))]);
        let font = Font::load(&doc, &dict).unwrap();
        // 0x0041 puis 0x00FF, plus un octet isolé en trop (silencieusement ignoré).
        assert_eq!(
            font.decode_composite(&[0x00, 0x41, 0x00, 0xFF, 0x9]),
            vec![0x0041, 0x00FF]
        );
    }

    /// `/W` mélange les deux formes autorisées (ISO 32000-1 §9.7.4.3) dans
    /// le même tableau : `c [w1 w2 ...]` (largeurs individuelles) et
    /// `cFirst cLast w` (plage uniforme). Un CID absent des deux doit
    /// retomber sur `/DW`.
    #[test]
    fn cid_widths_handle_both_w_array_forms_and_fall_back_to_dw() {
        let doc = dummy_doc();
        let cid_font_dict = font_dict(&[
            ("Subtype", Object::Name("CIDFontType2".into())),
            ("DW", Object::Integer(1000)),
            (
                "W",
                Object::Array(vec![
                    // CID 10 -> 200, CID 11 -> 300 (forme tableau).
                    Object::Integer(10),
                    Object::Array(vec![Object::Integer(200), Object::Integer(300)]),
                    // CID 20..22 -> 400 (forme plage uniforme).
                    Object::Integer(20),
                    Object::Integer(22),
                    Object::Integer(400),
                ]),
            ),
        ]);
        let type0_dict = font_dict(&[
            ("Subtype", Object::Name("Type0".into())),
            ("Encoding", Object::Name("Identity-H".into())),
            (
                "DescendantFonts",
                Object::Array(vec![Object::Dictionary(cid_font_dict)]),
            ),
        ]);
        let font = Font::load(&doc, &type0_dict).unwrap();

        assert_eq!(font.cid_metrics(10).1, 200.0);
        assert_eq!(font.cid_metrics(11).1, 300.0);
        assert_eq!(font.cid_metrics(20).1, 400.0);
        assert_eq!(font.cid_metrics(21).1, 400.0);
        assert_eq!(font.cid_metrics(22).1, 400.0);
        // Ni dans la forme tableau ni dans la plage -> repli sur /DW.
        assert_eq!(font.cid_metrics(999).1, 1000.0);
    }

    /// Symétrique de `to_unicode_bfchar_overrides_base_encoding`, pour un
    /// code source 2 octets (`>= 256`, rejeté par `parse_to_unicode_cmap`
    /// mais pas par la variante `_wide` des polices composites).
    #[test]
    fn to_unicode_wide_resolves_a_two_byte_source_code() {
        let doc = dummy_doc();
        let to_unicode_data =
            b"/CIDInit /ProcSet findresource begin\nbeginbfchar\n<4E2D> <4E2D>\nendbfchar\nend";
        let type0_dict = font_dict(&[
            ("Subtype", Object::Name("Type0".into())),
            ("Encoding", Object::Name("Identity-H".into())),
            (
                "ToUnicode",
                Object::Stream(crate::object::Stream {
                    dict: Dictionary::new(),
                    raw_data: to_unicode_data.to_vec(),
                }),
            ),
        ]);
        let font = Font::load(&doc, &type0_dict).unwrap();
        assert_eq!(font.cid_metrics(0x4E2D).0, Some('中'));
    }

    /// Bout en bout : un `/Type0`/`CIDFontType2` dont le descendant partage
    /// le même `/FontFile2` que le fixture TrueType simple existant
    /// (`embedded_truetype_font.pdf`), avec un `/CIDToGIDMap` explicite qui
    /// fait pointer un CID arbitraire vers le GID du glyphe 'A' — vérifie
    /// que la résolution CID -> GID -> contour fonctionne réellement,
    /// pas seulement que les champs sont lus.
    #[test]
    fn composite_truetype_font_resolves_cid_via_explicit_cidtogidmap() {
        let bytes = include_bytes!("../tests/fixtures/embedded_truetype_font.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let font_res = page.resources.get("Font").unwrap();
        let simple_font_dict = doc
            .get(font_res)
            .unwrap()
            .as_dict()
            .unwrap()
            .iter()
            .find_map(|(_, obj)| {
                let resolved = doc.get(obj).ok()?;
                let d = resolved.as_dict()?;
                (d.get("Subtype").and_then(|o| o.as_name()) == Some("TrueType")).then(|| d.clone())
            })
            .expect("expected an embedded TrueType font resource in the fixture");

        let descriptor_obj = simple_font_dict.get("FontDescriptor").unwrap().clone();
        let descriptor_dict = doc.get(&descriptor_obj).unwrap().as_dict().unwrap().clone();
        let font_file2_obj = descriptor_dict.get("FontFile2").unwrap().clone();
        let Object::Stream(font_file2_stream) = doc.get(&font_file2_obj).unwrap() else {
            panic!("expected a FontFile2 stream");
        };
        let raw_truetype = decode_stream(&font_file2_stream).unwrap();

        let gid_a = {
            let face = ttf_parser::Face::parse(&raw_truetype, 0).unwrap();
            face.glyph_index('A')
                .or_else(|| {
                    face.tables().cmap.and_then(|cmap| {
                        cmap.subtables
                            .into_iter()
                            .find(|sub| sub.platform_id == ttf_parser::PlatformId::Macintosh)
                            .and_then(|sub| sub.glyph_index(b'A' as u32))
                    })
                })
                .expect("expected a GID for 'A' in the Monaco subset")
                .0
        };

        // CID 9 (arbitraire) -> GID de 'A', via un flux `/CIDToGIDMap` explicite
        // (grand assez pour couvrir les CID 0..=9).
        let mut cid_to_gid_bytes = vec![0u8; 20];
        cid_to_gid_bytes[9 * 2] = (gid_a >> 8) as u8;
        cid_to_gid_bytes[9 * 2 + 1] = (gid_a & 0xff) as u8;

        let cid_font_dict = font_dict(&[
            ("Subtype", Object::Name("CIDFontType2".into())),
            ("FontDescriptor", descriptor_obj.clone()),
            ("DW", Object::Integer(1000)),
            (
                "W",
                Object::Array(vec![
                    Object::Integer(9),
                    Object::Array(vec![Object::Integer(600)]),
                ]),
            ),
            (
                "CIDToGIDMap",
                Object::Stream(crate::object::Stream {
                    dict: Dictionary::new(),
                    raw_data: cid_to_gid_bytes,
                }),
            ),
        ]);
        let to_unicode_data =
            b"/CIDInit /ProcSet findresource begin\nbeginbfchar\n<0009> <0041>\nendbfchar\nend";
        let type0_dict = font_dict(&[
            ("Subtype", Object::Name("Type0".into())),
            ("Encoding", Object::Name("Identity-H".into())),
            (
                "DescendantFonts",
                Object::Array(vec![Object::Dictionary(cid_font_dict)]),
            ),
            (
                "ToUnicode",
                Object::Stream(crate::object::Stream {
                    dict: Dictionary::new(),
                    raw_data: to_unicode_data.to_vec(),
                }),
            ),
        ]);

        let font = Font::load(&doc, &type0_dict).unwrap();
        assert!(font.is_composite());

        let (unicode, width) = font.cid_metrics(9);
        assert_eq!(unicode, Some('A'));
        assert_eq!(width, 600.0);

        let outline = font
            .cid_glyph_outline(9)
            .expect("expected an outline for CID 9 via the explicit CIDToGIDMap");
        assert!(!outline.is_empty());
        assert!(matches!(outline[0], PathSegment::MoveTo(_)));

        // Même contour que la résolution "police simple" du même glyphe
        // dans le fixture d'origine (même police, même GID).
        let simple_font = Font::load(&doc, &simple_font_dict).unwrap();
        let simple_outline = simple_font.glyph_outline(b'A', Some('A')).unwrap();
        assert_eq!(outline, simple_outline);
    }

    /// Ne s'exécute utilement que sur macOS (fichiers de
    /// `/System/Library/Fonts`) — la CI GitHub Actions tourne sur
    /// `macos-latest`, donc le chemin est couvert.
    #[test]
    #[cfg(target_os = "macos")]
    fn non_embedded_helvetica_gets_system_outline() {
        let doc = dummy_doc();
        let dict = font_dict(&[
            ("Subtype", Object::Name("Type1".into())),
            ("BaseFont", Object::Name("Helvetica".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        let outline = font
            .glyph_outline(b'A', Some('A'))
            .expect("system Helvetica substitution should provide an outline for 'A'");
        assert!(!outline.is_empty());
    }

    #[test]
    fn embedded_truetype_fixture_yields_real_glyph_outline() {
        let bytes = include_bytes!("../tests/fixtures/embedded_truetype_font.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let font_res = page.resources.get("Font").unwrap();
        let font_dict = doc.get(font_res).unwrap();
        let embedded = font_dict
            .as_dict()
            .unwrap()
            .iter()
            .find_map(|(name, obj)| {
                let resolved = doc.get(obj).ok()?;
                let d = resolved.as_dict()?;
                (d.get("Subtype").and_then(|o| o.as_name()) == Some("TrueType")).then(|| {
                    let _ = name;
                    d.clone()
                })
            })
            .expect("expected an embedded TrueType font resource in the fixture");

        let font = Font::load(&doc, &embedded).unwrap();
        let outline = font
            .glyph_outline(b'A', Some('A'))
            .expect("expected an outline for 'A' in the embedded Monaco subset");
        assert!(!outline.is_empty());
        assert!(matches!(outline[0], PathSegment::MoveTo(_)));

        // Le sous-ensemble Monaco de ce fixture n'embarque qu'un cmap
        // Macintosh (pas de table Unicode) : sans le repli code-brut, la
        // résolution par Unicode seul échouerait.
        let outline_via_code_only = font
            .glyph_outline(b'A', None)
            .expect("code-based cmap fallback should still resolve the glyph");
        assert_eq!(outline, outline_via_code_only);
    }

    #[test]
    fn embedded_cff_fixture_yields_real_glyph_outline() {
        let bytes = include_bytes!("../tests/fixtures/embedded_cff_font.pdf").to_vec();
        let doc = Document::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let font_res = page.resources.get("Font").unwrap();
        let font_dict = doc.get(font_res).unwrap();
        let font_entry = font_dict
            .as_dict()
            .unwrap()
            .iter()
            .next()
            .map(|(_, obj)| doc.get(obj).unwrap())
            .expect("expected a font resource in the fixture");
        let font_dict = font_entry.as_dict().unwrap();

        let font = Font::load(&doc, font_dict).unwrap();
        let outline = font
            .glyph_outline(b'A', Some('A'))
            .expect("expected a CFF outline for 'A' in the embedded STIX subset");
        assert!(!outline.is_empty());
        assert!(matches!(outline[0], PathSegment::MoveTo(_)));
    }

    #[test]
    fn to_unicode_bfchar_overrides_base_encoding() {
        let doc = dummy_doc();
        let cmap = b"1 beginbfchar\n<41> <0042>\nendbfchar\n";
        let to_unicode = Object::Stream(crate::object::Stream {
            dict: Dictionary::new(),
            raw_data: cmap.to_vec(),
        });
        let dict = font_dict(&[
            ("Subtype", Object::Name("TrueType".into())),
            ("Encoding", Object::Name("WinAnsiEncoding".into())),
            ("ToUnicode", to_unicode),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        // Sans /ToUnicode, code 0x41 ('A') résoudrait vers 'A' via WinAnsi ;
        // le CMap le fait pointer vers 'B' à la place.
        let (unicode, _) = font.decode_simple(0x41);
        assert_eq!(unicode, Some('B'));
    }

    #[test]
    fn to_unicode_bfrange_with_base_increments_across_the_range() {
        let doc = dummy_doc();
        let cmap = b"1 beginbfrange\n<01> <03> <0041>\nendbfrange\n";
        let to_unicode = Object::Stream(crate::object::Stream {
            dict: Dictionary::new(),
            raw_data: cmap.to_vec(),
        });
        let dict = font_dict(&[
            ("Subtype", Object::Name("TrueType".into())),
            ("ToUnicode", to_unicode),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        assert_eq!(font.decode_simple(0x01).0, Some('A'));
        assert_eq!(font.decode_simple(0x02).0, Some('B'));
        assert_eq!(font.decode_simple(0x03).0, Some('C'));
    }

    #[test]
    fn to_unicode_bfrange_with_explicit_array() {
        let doc = dummy_doc();
        let cmap = b"1 beginbfrange\n<01> <02> [<0058> <0059>]\nendbfrange\n";
        let to_unicode = Object::Stream(crate::object::Stream {
            dict: Dictionary::new(),
            raw_data: cmap.to_vec(),
        });
        let dict = font_dict(&[
            ("Subtype", Object::Name("TrueType".into())),
            ("ToUnicode", to_unicode),
        ]);
        let font = Font::load(&doc, &dict).unwrap();
        assert_eq!(font.decode_simple(0x01).0, Some('X'));
        assert_eq!(font.decode_simple(0x02).0, Some('Y'));
    }

    /// Bout en bout sur le fixture CJK réel : sans `/ToUnicode`, aucun
    /// caractère n'était récupéré pour ce texte (voir STATUS.md, limitation
    /// documentée avant l'ajout de ce parseur). Avec, le texte exact doit
    /// être reconstruit.
    #[test]
    fn real_cjk_fixture_recovers_unicode_via_to_unicode_cmap() {
        use crate::interp::Interpreter;
        use crate::Document as CoreDocument;

        let bytes = include_bytes!("../tests/fixtures/cjk_text.pdf").to_vec();
        let doc = CoreDocument::open(bytes).unwrap();
        let page = doc.page(0).unwrap();
        let content = doc.page_content(&page).unwrap();
        let display = Interpreter::run_page(&doc, page.resources.clone(), &content).unwrap();

        let text: String = display
            .items
            .iter()
            .filter_map(|item| match item {
                crate::display::DisplayItem::Glyph { unicode, .. } => *unicode,
                _ => None,
            })
            .collect();
        assert_eq!(text, "你好，世界");
    }
}
