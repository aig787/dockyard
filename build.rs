extern crate vergen;

use vergen::{ConstantsFlags, generate_cargo_keys};

fn main() {
    // Setup the flags, toggling off the 'SEMVER_FROM_CARGO_PKG' flag
    let flags = ConstantsFlags::SEMVER_FROM_CARGO_PKG
        | ConstantsFlags::SHA;

    // Generate the 'cargo:' key output
    generate_cargo_keys(flags).expect("Unable to generate the cargo keys!");
}