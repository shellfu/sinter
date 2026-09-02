pub mod util;

use crate::util as u;

/// The alias binds a module and the tail names a type's member. Nothing is
/// missing from the corpus — resolution just does not follow a module alias
/// through a two-segment tail, and records that limitation instead of
/// leaving the miss to look like a dangling reference.
pub fn assist_through_alias() {
    u::Helper::assist();
}
