//! 010 Editor Binary Templates: a `.bt` template describes a binary format in a
//! C-like language, and running it over a file maps the file's bytes to a tree
//! of named, typed variables — the hex editor's template panel.
//!
//! - [`bundle`] / [`library`]: the templates shipped with the program, deployed
//!   to the config directory, and auto-selected for a file by [`header`] masks
//!   and ID bytes.
//! - [`lex`] → [`preproc`] → [`parse`]: source to [`ast`].
//! - [`interp`]: runs a program over a [`source::ByteSource`], producing a
//!   [`tree::Tree`] of [`value`]s read from the file only when shown.
//! - [`colors`]: the template's colours as tints of the theme.

pub mod ast;
pub mod bundle;
pub mod colors;
pub mod header;
pub mod interp;
pub mod lex;
pub mod library;
pub mod parse;
pub mod preproc;
pub mod source;
pub mod tree;
pub mod value;

#[cfg(test)]
mod corpus;
