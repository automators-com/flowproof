//! One submodule per requirement/test-case document export format that
//! `doc_author` can draft from. Each format owns its own parser and
//! error type; `doc_author` picks a format module by extension/heuristic
//! today (only one exists) and would gain a `--format` flag the day a
//! second one lands, not before.

pub mod hp_alm;
