//! Moteur PDF maison — voir architecture.md pour la vue d'ensemble.
//!
//! Couches implémentées à ce stade (Sprint 1-4, voir sprint.md) :
//! lexer -> objets COS -> xref/trailer -> modèle document minimal.
//!
//! Non encore implémenté (sprints ultérieurs) : object streams / xref
//! streams (PDF 1.5+), filtres autres que ceux listés dans `filters`,
//! interpréteur de flux de contenu, rendu, polices, édition.

pub mod content;
pub mod crypt;
pub mod display;
pub mod document;
pub mod encoding;
pub mod error;
pub mod filters;
pub mod font;
pub mod image;
pub mod interp;
pub mod lexer;
pub mod object;
pub mod outline;
pub mod page;
pub mod parser;
pub mod writer;
pub mod xref;

pub use document::Document;
pub use error::{PdfError, Result};
pub use object::{Dictionary, Name, ObjRef, Object, Stream};
pub use outline::OutlineItem;
pub use page::Page;
