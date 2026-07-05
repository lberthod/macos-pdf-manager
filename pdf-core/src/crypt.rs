//! Déchiffrement `/Encrypt` (gestionnaire de sécurité standard, ISO 32000-1
//! §7.6 pour les révisions 2-4 (RC4/AES-128), ISO 32000-2 §7.6.4 — extension
//! Adobe niveau 3 pour la révision 5 — pour la révision 6 (AES-256), Sprint
//! 22, `audit50quest.md` #50.
//!
//! Portée volontairement restreinte : seul le mot de passe utilisateur
//! **vide** est géré (le cas réel le plus courant — un PDF "protégé" dont
//! l'ouverture n'est en réalité soumise à aucun mot de passe, seules les
//! permissions d'édition/impression sont restreintes côté lecteurs qui les
//! respectent). Aucune UI de saisie de mot de passe n'existe encore côté
//! `pdf-app`/`pdf-ui` ; un document nécessitant un vrai mot de passe utilisateur
//! reste donc illisible (le déchiffrement avec la mauvaise clé produit du
//! contenu corrompu plutôt qu'une erreur explicite — limitation connue).
//!
//! Primitives cryptographiques (MD5/SHA-2/RC4/AES) déléguées à des crates
//! auditées (`md-5`, `sha2`, `rc4`, `aes`, `cbc`) plutôt que réimplémentées à
//! la main : ce ne sont pas des briques spécifiques à un moteur PDF, au même
//! titre que `zune-jpeg` pour le décodage JPEG.

use crate::object::{Dictionary, Object, Stream};

use aes::{Aes128, Aes256};
use cbc::cipher::block_padding::{NoPadding, Pkcs7};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::{Digest as _, Md5};
use sha2::{Sha256, Sha384, Sha512};

/// Chaîne de remplissage standard (ISO 32000-1 Algorithme 2, étape a) —
/// utilisée pour compléter un mot de passe à 32 octets ; un mot de passe
/// vide est donc remplacé exactement par cette chaîne.
const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CryptMethod {
    Rc4,
    AesV2, // AES-128-CBC
    AesV3, // AES-256-CBC
    Identity,
}

/// Contexte de déchiffrement d'un document — construit une fois à
/// l'ouverture (`Document::open`), réutilisé pour chaque objet résolu.
pub struct Decryptor {
    file_key: Vec<u8>,
    method: CryptMethod,
    /// Version de l'algorithme (`/V`) : détermine si la clé par objet doit
    /// être dérivée (`V` <= 4, Algorithme 1) ou si `file_key` sert
    /// directement de clé pour tous les objets (`V` == 5).
    v: i64,
    /// Numéro de l'objet `/Encrypt` lui-même — jamais déchiffré (ses
    /// chaînes, notamment `/O`/`/U`/`/OE`/`/UE`, sont en clair par
    /// construction).
    encrypt_obj_num: Option<u32>,
}

impl Decryptor {
    /// Construit le contexte de déchiffrement à partir du dictionnaire
    /// `/Encrypt` (déjà résolu, non chiffré) et du premier élément de
    /// `/ID` du trailer (ISO 32000-1 exige `/ID` pour les révisions <= 4).
    /// Suppose un mot de passe utilisateur vide (voir la doc de module).
    pub fn new(encrypt: &Dictionary, id0: &[u8], encrypt_obj_num: Option<u32>) -> Option<Self> {
        if encrypt.get("Filter").and_then(|o| o.as_name()) != Some("Standard") {
            return None; // gestionnaire de sécurité non standard : non géré.
        }
        let v = encrypt.get("V").and_then(|o| o.as_int()).unwrap_or(0);
        let r = encrypt.get("R").and_then(|o| o.as_int())?;
        let o_entry = string_bytes(encrypt.get("O")?)?;
        let u_entry = string_bytes(encrypt.get("U")?)?;
        let p = encrypt.get("P").and_then(|o| o.as_int())? as i32;

        if v <= 4 {
            let length_bits = encrypt.get("Length").and_then(|o| o.as_int()).unwrap_or(40);
            let key_len = if r == 2 {
                5
            } else {
                (length_bits / 8) as usize
            };
            let encrypt_metadata = encrypt
                .get("EncryptMetadata")
                .map(|o| matches!(o, Object::Boolean(true)))
                .unwrap_or(true);

            let mut input = Vec::with_capacity(32 + 32 + 4 + id0.len() + 4);
            input.extend_from_slice(&PAD); // mot de passe utilisateur vide, complété.
            input.extend_from_slice(&o_entry[..32.min(o_entry.len())]);
            input.extend_from_slice(&p.to_le_bytes());
            input.extend_from_slice(id0);
            if r >= 4 && !encrypt_metadata {
                input.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            }
            let mut hash: [u8; 16] = Md5::digest(&input).into();
            if r >= 3 {
                for _ in 0..50 {
                    hash = Md5::digest(&hash[..key_len.min(16)]).into();
                }
            }
            let file_key = hash[..key_len.min(16)].to_vec();

            let method = if v == 4 {
                crypt_filter_method(encrypt).unwrap_or(CryptMethod::Rc4)
            } else {
                CryptMethod::Rc4
            };

            Some(Self {
                file_key,
                method,
                v,
                encrypt_obj_num,
            })
        } else {
            // V5 (R5/R6, AES-256) : voir Algorithme 2.A (ISO 32000-2 §7.6.4.3.3)
            // pour retrouver la clé de fichier à partir du mot de passe
            // utilisateur — ici toujours vide.
            if u_entry.len() < 48 {
                return None;
            }
            let key_salt = &u_entry[40..48];
            let ue = string_bytes(encrypt.get("UE")?)?;
            if ue.len() != 32 {
                return None;
            }
            let intermediate = if r >= 6 {
                hardened_hash(&[], key_salt, &[])
            } else {
                let mut hasher = Sha256::new();
                hasher.update(key_salt);
                hasher.finalize().to_vec()
            };
            let mut key = [0u8; 32];
            key.copy_from_slice(&intermediate);
            let mut iv = [0u8; 16];
            let file_key = aes256_cbc_decrypt_no_padding(&key, &iv, &ue);
            iv.fill(0);

            Some(Self {
                file_key,
                method: CryptMethod::AesV3,
                v,
                encrypt_obj_num,
            })
        }
    }

    pub fn encrypt_obj_num(&self) -> Option<u32> {
        self.encrypt_obj_num
    }

    /// Clé de déchiffrement de l'objet `(num, gen)` — dérivée de la clé de
    /// fichier (Algorithme 1, ISO 32000-1 §7.6.2) pour `V` <= 4 ; identique
    /// à la clé de fichier pour `V` == 5 (pas de dérivation par objet).
    fn object_key(&self, num: u32, gen: u16) -> Vec<u8> {
        if self.v >= 5 {
            return self.file_key.clone();
        }
        let mut input = self.file_key.clone();
        input.extend_from_slice(&num.to_le_bytes()[..3]);
        input.extend_from_slice(&gen.to_le_bytes()[..2]);
        if self.method == CryptMethod::AesV2 {
            input.extend_from_slice(b"sAlT");
        }
        let hash: [u8; 16] = Md5::digest(&input).into();
        let len = (self.file_key.len() + 5).min(16);
        hash[..len].to_vec()
    }

    fn decrypt_bytes(&self, data: &[u8], num: u32, gen: u16) -> Vec<u8> {
        if data.is_empty() {
            return Vec::new();
        }
        let key = self.object_key(num, gen);
        match self.method {
            CryptMethod::Identity => data.to_vec(),
            CryptMethod::Rc4 => rc4_apply(&key, data),
            CryptMethod::AesV2 => aes_cbc_decrypt_pkcs7(&key, data, false),
            CryptMethod::AesV3 => aes_cbc_decrypt_pkcs7(&key, data, true),
        }
    }

    /// Déchiffre récursivement toutes les chaînes/flux contenus dans
    /// `object` (qui vient d'être résolu à `(num, gen)`) — appelé par
    /// `Document::resolve` juste après le parsing, avant la mise en cache.
    pub fn decrypt_object(&self, object: &mut Object, num: u32, gen: u16) {
        match object {
            Object::String(bytes) => {
                *bytes = self.decrypt_bytes(bytes, num, gen);
            }
            Object::Array(items) => {
                for item in items {
                    self.decrypt_object(item, num, gen);
                }
            }
            Object::Dictionary(dict) => {
                for value in dict.0.values_mut() {
                    self.decrypt_object(value, num, gen);
                }
            }
            Object::Stream(stream) => {
                let Stream { dict, raw_data } = stream;
                for value in dict.0.values_mut() {
                    self.decrypt_object(value, num, gen);
                }
                *raw_data = self.decrypt_bytes(raw_data, num, gen);
            }
            Object::Null
            | Object::Boolean(_)
            | Object::Integer(_)
            | Object::Real(_)
            | Object::Name(_)
            | Object::Reference(_) => {}
        }
    }
}

fn string_bytes(o: &Object) -> Option<Vec<u8>> {
    match o {
        Object::String(bytes) => Some(bytes.clone()),
        _ => None,
    }
}

/// Méthode de chiffrement effective pour `V` == 4 : nom du filtre référencé
/// par `/StrF` (ou `/StmF`, les deux pointent en pratique vers le même
/// filtre nommé pour tous les fixtures rencontrés) puis son `/CFM` dans
/// `/CF`.
fn crypt_filter_method(encrypt: &Dictionary) -> Option<CryptMethod> {
    let filter_name = encrypt
        .get("StmF")
        .and_then(|o| o.as_name())
        .unwrap_or("Identity");
    if filter_name == "Identity" {
        return Some(CryptMethod::Identity);
    }
    let cf = encrypt.get("CF")?.as_dict()?;
    let filter = cf.get(filter_name)?.as_dict()?;
    match filter.get("CFM").and_then(|o| o.as_name()) {
        Some("AESV2") => Some(CryptMethod::AesV2),
        Some("AESV3") => Some(CryptMethod::AesV3),
        Some("V2") => Some(CryptMethod::Rc4),
        Some("None") => Some(CryptMethod::Identity),
        _ => None,
    }
}

/// RC4 (symétrique : la même opération chiffre et déchiffre) — implémenté à
/// la main plutôt que via une crate : l'algorithme (KSA + PRGA) est
/// trivial et bien connu, et RC4 est de toute façon un chiffrement obsolète
/// qu'aucune crate ne "sécurise" davantage qu'une implémentation directe
/// correcte ; on ne le supporte ici que pour la compatibilité en lecture de
/// vieux PDF, jamais pour protéger quoi que ce soit.
fn rc4_apply(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut s: [u8; 256] = [0; 256];
    for (i, entry) in s.iter_mut().enumerate() {
        *entry = i as u8;
    }
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len().max(1)]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let (mut i, mut j) = (0u8, 0u8);
    for &byte in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(byte ^ k);
    }
    out
}

fn aes256_cbc_decrypt_no_padding(key: &[u8; 32], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    let mut buf = data.to_vec();
    let decryptor = cbc::Decryptor::<Aes256>::new(key.into(), iv.into());
    decryptor
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map(|out| out.to_vec())
        .unwrap_or(buf)
}

/// Déchiffre un flux/chaîne AES (ISO 32000-1 §7.6.2 : IV de 16 octets en
/// tête des données, PKCS#7 pour le bourrage — contrairement au décodage
/// "brut" sans bourrage utilisé pour `/OE`/`/UE` en révision 5/6).
fn aes_cbc_decrypt_pkcs7(key: &[u8], data: &[u8], aes256: bool) -> Vec<u8> {
    if data.len() < 16 {
        return Vec::new();
    }
    let (iv, ciphertext) = data.split_at(16);
    let mut buf = ciphertext.to_vec();
    let plaintext = if aes256 && key.len() == 32 {
        let key_arr: [u8; 32] = key.try_into().unwrap();
        cbc::Decryptor::<Aes256>::new(&key_arr.into(), iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map(|out| out.len())
    } else if key.len() == 16 {
        let key_arr: [u8; 16] = key.try_into().unwrap();
        cbc::Decryptor::<Aes128>::new(&key_arr.into(), iv.into())
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .map(|out| out.len())
    } else {
        return Vec::new();
    };
    match plaintext {
        Ok(len) => {
            buf.truncate(len);
            buf
        }
        Err(_) => Vec::new(),
    }
}

/// Algorithme 2.B (ISO 32000-2 Annexe C, "hardened hash") — hachage itératif
/// utilisé pour la révision 6 (contrairement à la révision 5, qui utilise
/// un simple SHA-256). `input` = mot de passe (toujours vide ici), `salt` =
/// sel de clé ou de validation (8 octets), `udata` = données utilisateur
/// supplémentaires (vide pour le mot de passe utilisateur).
fn hardened_hash(password: &[u8], salt: &[u8], udata: &[u8]) -> Vec<u8> {
    let mut k: Vec<u8> = {
        let mut hasher = Sha256::new();
        hasher.update(password);
        hasher.update(salt);
        hasher.update(udata);
        hasher.finalize().to_vec()
    };

    let mut round = 0u32;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + udata.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(udata);
        }

        let key: [u8; 16] = k[0..16].try_into().unwrap();
        let iv: [u8; 16] = k[16..32].try_into().unwrap();
        let mut buf = k1.clone();
        let encryptor = cbc::Encryptor::<Aes128>::new(&key.into(), &iv.into());
        let e_len = encryptor
            .encrypt_padded_mut::<NoPadding>(&mut buf, k1.len())
            .map(|out| out.len())
            .unwrap_or(0);
        let e = &buf[..e_len];

        let modulus: u32 = e[..16].iter().map(|&b| b as u32).sum::<u32>() % 3;
        k = match modulus {
            0 => Sha256::digest(e).to_vec(),
            1 => Sha384::digest(e).to_vec(),
            _ => Sha512::digest(e).to_vec(),
        };

        round += 1;
        if round >= 64 && (*e.last().unwrap_or(&0) as u32) <= round - 32 {
            break;
        }
    }

    k.truncate(32);
    k
}
