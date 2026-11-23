//! Rebuilds the crate when the built-in OAuth credentials change: they are
//! read with `option_env!` (see `src/settings.rs`).

fn main() {
    for var in [
        "NMMAIL_GOOGLE_CLIENT_ID",
        "NMMAIL_GOOGLE_CLIENT_SECRET",
        "NMMAIL_MICROSOFT_CLIENT_ID",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
