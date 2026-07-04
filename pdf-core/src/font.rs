//! Métriques et encodage des polices simples (Type1/TrueType 1 octet) —
//! architecture.md §4.6. Fournit à l'interpréteur de contenu (`interp.rs`)
//! de quoi remplacer l'avance de texte placeholder par une largeur réelle,
//! et le code de caractère brut par un caractère Unicode.
//!
//! Limitations connues (voir sprint.md, Sprint 7-8+) :
//! - Polices composites (`/Type0`, CID, codes 2 octets) : non gérées, un
//!   appelant doit détecter `Font::is_composite()` et garder son
//!   comportement de repli.
//! - `/ToUnicode` (CMap dédié, ISO 32000-1 §9.10.3) : lu et prioritaire sur
//!   l'encodage de base (`base_encoding_by_name`/`/Differences`) quand
//!   présent — voir `parse_to_unicode_cmap`. Gère `beginbfchar`/
//!   `beginbfrange` (forme "base + décalage" et forme tableau explicite) ;
//!   seuls les codes source tenant sur un octet (0..256) sont retenus,
//!   cohérent avec le reste de ce module qui ne traite que les polices
//!   simples. Les valeurs de destination multi-unités UTF-16 (paires de
//!   substitution, plusieurs caractères) ne sont pas incrémentées dans une
//!   plage `bfrange` : toute la plage reçoit alors la même valeur
//!   (approximation rare en pratique).
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

pub struct Font {
    subtype: String,
    encoding: [Option<char>; 256],
    widths: [Option<f64>; 256],
    use_helvetica_fallback: bool,
    /// Octets bruts d'un `/FontFile2` (TrueType) déjà décodé, s'il y en a un.
    embedded_truetype: Option<Vec<u8>>,
    /// Octets bruts d'un `/FontFile3` `/Subtype /Type1C` (CFF brut, sans
    /// conteneur OpenType) déjà décodé, s'il y en a un.
    embedded_cff: Option<Vec<u8>>,
    /// Police système de substitution (octets du fichier partagés via cache
    /// global + index de face dans la collection `.ttc`), pour les polices
    /// standard non intégrées. `None` si la police est intégrée ou si aucune
    /// substitution n'a été trouvée.
    system_fallback: Option<(std::sync::Arc<Vec<u8>>, u32)>,
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

        let use_helvetica_fallback = subtype != "Type0" && widths.iter().all(|w| w.is_none());

        let descriptor_dict = dict
            .get("FontDescriptor")
            .and_then(|o| doc.get(o).ok())
            .and_then(|desc| desc.as_dict().cloned());

        let mut embedded_truetype = descriptor_dict
            .as_ref()
            .and_then(|d| d.get("FontFile2").cloned())
            .and_then(|obj| doc.get(&obj).ok())
            .and_then(|obj| match obj {
                Object::Stream(stream) => decode_stream(&stream).ok(),
                _ => None,
            });

        let mut embedded_cff = None;
        if embedded_truetype.is_none() {
            if let Some(Object::Stream(stream)) = descriptor_dict
                .as_ref()
                .and_then(|d| d.get("FontFile3").cloned())
                .and_then(|obj| doc.get(&obj).ok())
            {
                let file_subtype = stream.dict.get("Subtype").and_then(|o| o.as_name());
                if let Ok(bytes) = decode_stream(&stream) {
                    match file_subtype {
                        // Conteneur OpenType complet : ttf-parser::Face
                        // gère indifféremment glyf et CFF en interne.
                        Some("OpenType") => embedded_truetype = Some(bytes),
                        // CFF brut (`Type1C`/`CIDFontType0C`) : pas de
                        // table sfnt, nécessite le parseur CFF dédié.
                        _ => embedded_cff = Some(bytes),
                    }
                }
            }
        }

        let system_fallback =
            if embedded_truetype.is_none() && embedded_cff.is_none() && subtype != "Type0" {
                let base_font = dict
                    .get("BaseFont")
                    .and_then(|o| o.as_name())
                    .unwrap_or("Helvetica");
                system_font::lookup(base_font)
            } else {
                None
            };

        Ok(Font {
            subtype,
            encoding,
            widths,
            use_helvetica_fallback,
            embedded_truetype,
            embedded_cff,
            system_fallback,
        })
    }

    /// Décode un code de caractère 1 octet (polices simples uniquement) en
    /// `(unicode résolu, largeur en 1/1000 em)`.
    pub fn decode_simple(&self, code: u8) -> (Option<char>, f64) {
        let unicode = self.encoding[code as usize];
        let width = self.widths[code as usize]
            .or_else(|| {
                if self.use_helvetica_fallback {
                    helvetica_width(code)
                } else {
                    None
                }
            })
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

fn base_encoding_by_name(name: &str) -> Option<[Option<char>; 256]> {
    match name {
        "WinAnsiEncoding" => Some(WIN_ANSI_ENCODING),
        "StandardEncoding" => Some(STANDARD_ENCODING),
        "MacRomanEncoding" => Some(WIN_ANSI_ENCODING), // approximation, voir limitations ci-dessus.
        _ => None,
    }
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
        _ => return None,
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
