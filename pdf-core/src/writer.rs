//! Sérialisation d'`Object` en syntaxe PDF — architecture.md §4.2, symétrique
//! du lexer/parser (Sprint 1-4). Nécessaire à partir du Sprint 13-14 pour la
//! sauvegarde incrémentale (`document::save_incremental`) et la génération
//! de flux d'apparence (annotations, champs de formulaire).
//!
//! Partis pris pour rester simple et toujours correct plutôt que compact :
//! - Les chaînes (`Object::String`) sont toujours écrites en **chaîne
//!   hexadécimale** (`<...>`) plutôt qu'en littéral échappé — évite toute
//!   logique d'échappement de parenthèses/antislash, valide pour n'importe
//!   quelle séquence d'octets (texte, UTF-16BE avec BOM, binaire).
//! - Les noms (`Object::Name`) échappent en `#XX` tout octet en dehors de
//!   l'ASCII imprimable sans délimiteur PDF (ISO 32000-1 §7.3.5) plutôt que
//!   de supposer que le nom est déjà "propre".
//! - Les flux nouvellement créés par ce moteur (annotations, apparences) ne
//!   portent jamais de `/Filter` : les données sont écrites telles quelles.
//!   Un flux relu depuis un PDF existant (`Stream::raw_data`) est réécrit
//!   sans être redécodé/recompressé : c'est appelant (`document.rs`) qui
//!   décide s'il faut ou non toucher à un flux existant.

use crate::object::{Dictionary, Object};

/// Écrit `obj` en syntaxe PDF dans `out`. Ne gère pas les références
/// indirectes de haut niveau (`N G obj ... endobj`) — voir
/// `write_indirect_object` pour ça.
pub fn write_object(obj: &Object, out: &mut Vec<u8>) {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Object::Real(f) => out.extend_from_slice(format_real(*f).as_bytes()),
        Object::String(bytes) => write_hex_string(bytes, out),
        Object::Name(name) => write_name(name, out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_object(item, out);
            }
            out.push(b']');
        }
        Object::Dictionary(dict) => write_dictionary(dict, out),
        Object::Stream(stream) => {
            write_dictionary(&stream.dict, out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&stream.raw_data);
            out.extend_from_slice(b"\nendstream");
        }
        Object::Reference(r) => {
            out.extend_from_slice(format!("{} {} R", r.num, r.gen).as_bytes());
        }
    }
}

fn write_dictionary(dict: &Dictionary, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (key, value) in dict.iter() {
        out.push(b' ');
        write_name(key, out);
        out.push(b' ');
        write_object(value, out);
    }
    out.extend_from_slice(b" >>");
}

fn write_name(name: &str, out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in name.as_bytes() {
        let is_regular = b > 0x20
            && b < 0x7f
            && !matches!(
                b,
                b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'%' | b'#'
            );
        if is_regular {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}

fn write_hex_string(bytes: &[u8], out: &mut Vec<u8>) {
    out.push(b'<');
    for &b in bytes {
        out.extend_from_slice(format!("{b:02X}").as_bytes());
    }
    out.push(b'>');
}

/// Évite la notation scientifique (`1e10`), invalide en syntaxe PDF pour un
/// nombre réel — `ryu`/`{}` de Rust peut y basculer pour des valeurs
/// extrêmes, donc on formate nous-mêmes avec un nombre de décimales fixe
/// (6, largement suffisant pour des coordonnées de page) puis on retire les
/// zéros de fin superflus.
fn format_real(f: f64) -> String {
    let mut s = format!("{f:.6}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// Écrit un objet indirect complet : `N G obj\n...\nendobj\n`.
pub fn write_indirect_object(num: u32, gen: u16, obj: &Object, out: &mut Vec<u8>) {
    out.extend_from_slice(format!("{num} {gen} obj\n").as_bytes());
    write_object(obj, out);
    out.extend_from_slice(b"\nendobj\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{ObjRef, Stream};

    fn to_string(obj: &Object) -> String {
        let mut out = Vec::new();
        write_object(obj, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn writes_scalars() {
        assert_eq!(to_string(&Object::Null), "null");
        assert_eq!(to_string(&Object::Boolean(true)), "true");
        assert_eq!(to_string(&Object::Integer(-42)), "-42");
        assert_eq!(to_string(&Object::Real(1.5)), "1.5");
        assert_eq!(to_string(&Object::Real(2.0)), "2");
    }

    #[test]
    fn writes_string_as_hex() {
        assert_eq!(to_string(&Object::String(b"Hi".to_vec())), "<4869>");
    }

    #[test]
    fn writes_name_escaping_irregular_bytes() {
        assert_eq!(
            to_string(&Object::Name("Helvetica".to_string())),
            "/Helvetica"
        );
        assert_eq!(to_string(&Object::Name("A B".to_string())), "/A#20B");
    }

    #[test]
    fn writes_array_and_dictionary() {
        let arr = Object::Array(vec![Object::Integer(1), Object::Integer(2)]);
        assert_eq!(to_string(&arr), "[1 2]");

        let mut dict = Dictionary::new();
        dict.insert("Type", Object::Name("Page".to_string()));
        assert_eq!(to_string(&Object::Dictionary(dict)), "<< /Type /Page >>");
    }

    #[test]
    fn writes_reference() {
        assert_eq!(to_string(&Object::Reference(ObjRef::new(7, 0))), "7 0 R");
    }

    #[test]
    fn writes_stream_with_dict_and_raw_data() {
        let mut dict = Dictionary::new();
        dict.insert("Length", Object::Integer(5));
        let stream = Object::Stream(Stream {
            dict,
            raw_data: b"hello".to_vec(),
        });
        assert_eq!(
            to_string(&stream),
            "<< /Length 5 >>\nstream\nhello\nendstream"
        );
    }

    #[test]
    fn writes_full_indirect_object() {
        let mut out = Vec::new();
        write_indirect_object(3, 0, &Object::Integer(42), &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "3 0 obj\n42\nendobj\n");
    }
}
