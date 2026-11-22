//! The data model of NibKeep: entities described by statements, each one value of one
//! field with its qualifiers; the operations that change which statements are current;
//! and types, which list the fields an entity may have.
//!
//! The formats and rules are specified in `docs/spec/model.md`.

pub mod fields;
mod kinds;
mod language;
pub mod migration;
mod name;
mod qualifiers;
pub mod schema;
mod state;
mod statement;
pub mod storage;
mod types;
mod value;

pub use kinds::{Decimal, Name, ParseValueError, Quantity};
pub use language::{Languages, ParseLanguagesError, Tag};
pub use name::{FieldName, MigrationName, ParseNameError, TypeName};
pub use qualifiers::Qualifiers;
pub use schema::{DefinitionError, Types};
pub use state::{Counted, Resolution, State, content, presence_on};
pub use statement::{Operation, Statement, UnknownOperation};
pub use types::{FieldDef, Fit, Labels, Multiplicity, OtherMultiplicity, TypeDef};
pub use value::{OtherKind, UnknownValue, Value, ValueKind};
