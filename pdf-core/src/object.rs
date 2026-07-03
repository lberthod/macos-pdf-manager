//! Modèle d'objets COS (Carousel Object System) — architecture.md §4.2.

use crate::error::{PdfError, Result};
use std::collections::BTreeMap;

pub type Name = String;

/// Référence indirecte « N G R ».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub gen: u16,
}

impl ObjRef {
    pub fn new(num: u32, gen: u16) -> Self {
        Self { num, gen }
    }
}

/// Dictionnaire PDF : map ordonnée Name -> Object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Dictionary(pub BTreeMap<Name, Object>);

impl Dictionary {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn get(&self, key: &str) -> Option<&Object> {
        self.0.get(key)
    }

    pub fn insert(&mut self, key: impl Into<Name>, value: Object) {
        self.0.insert(key.into(), value);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Name, &Object)> {
        self.0.iter()
    }

    /// Récupère une valeur entière requise (n'essaie pas de résoudre les références).
    pub fn get_int(&self, key: &str) -> Result<i64> {
        match self.get(key) {
            Some(Object::Integer(n)) => Ok(*n),
            Some(_) => Err(PdfError::UnexpectedType("Integer")),
            None => Err(PdfError::MissingKey(key.to_string())),
        }
    }
}

/// Flux PDF : dictionnaire + données brutes (avant application des filtres).
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dictionary,
    pub raw_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(Name),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjRef),
}

impl Object {
    pub fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Object::Integer(n) => Some(*n),
            Object::Real(f) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match self {
            Object::Name(n) => Some(n.as_str()),
            _ => None,
        }
    }

    pub fn as_reference(&self) -> Option<ObjRef> {
        match self {
            Object::Reference(r) => Some(*r),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }
}
