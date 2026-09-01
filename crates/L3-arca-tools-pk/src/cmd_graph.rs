//! `graph` (dispatch del comando) + `pack` + `verify` + `trust-ring`.

use std::path::Path;

use arca_types::Res;

/// Comando `graph --src <dir> [--check-only]`.
pub(crate) fn run(src: &Path, check_only: bool) -> Res<()> {
    crate::graph::cmd(src, check_only)
}
