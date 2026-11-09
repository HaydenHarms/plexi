# Plexi

Rust notes/ink app with a transparent draw-on-screen overlay. Windows first.

- Workspace: `crates/plexi-core` (model, ink geometry, `.plexi` format, exporters — no UI)
  and `crates/plexi` (eframe/egui 0.37, glow renderer).
- Before committing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`. CI runs the same.
- All Win32 calls live in `crates/plexi/src/platform.rs`.
- Every visual export goes through `export::svg::frame_svg`, one call per `export::layout::Frame`
  (output page/file); the export dialog previews the same frames; keep the editor's rendering and
  that SVG in agreement (text metrics are pinned by `canvas::tests::text_metrics_*`).
- The window is created transparent once; notebook mode paints an opaque background, overlay
  mode paints only ink. Mode switches use viewport commands plus `platform::place_*`.
- `PLEXI_DATA_DIR` points the app at a throwaway data folder for testing.
- Release: bump `[workspace.package] version`, write `release notes/vX.Y.Z.md` (it heads the
  GitHub release), push tag `vX.Y.Z`.
