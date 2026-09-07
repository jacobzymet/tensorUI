# Local security backport

Source: crates.io glib 0.18.5 (MIT; see LICENSE).
Original crate archive SHA-256: `233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5`.
The only source change is `VariantStrIter::impl_get`: make the out pointer mutable
and pass `&mut p` to `g_variant_get_child`.

This backports https://github.com/gtk-rs/gtk-rs-core/pull/1343 to the GTK3-compatible
branch and addresses https://rustsec.org/advisories/RUSTSEC-2024-0429.html.
Keep the upstream version unchanged so the local backport is visible, not disguised
as a newer release. Cargo.toml patches crates.io to this directory.

On Linux with GLib development libraries installed, run the upstream iterator tests
with optimizations (the original undefined behavior depends on optimization):

`cargo test --manifest-path vendor/glib/Cargo.toml --release variant_str_iter`

Remove this override when the desktop stack supports maintained GLib bindings.
