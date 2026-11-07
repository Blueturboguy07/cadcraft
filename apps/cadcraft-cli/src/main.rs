#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

// publik's usage counts. The file is publik's reference client, kept as it is.
#[rustfmt::skip]
mod publik_telemetry;

/// The version in publik's usage counts: the release tag without its `v`, for example `0.18.0-publik.6`.
/// The build number comes from `packaging/PUBLIK_BUILD`, the file the bundle version comes from.
fn publik_version() -> String {
    format!("{}-publik.{}", env!("CARGO_PKG_VERSION"), include_str!("../../../packaging/PUBLIK_BUILD").trim())
}

fn main() {
    // publik's usage counts (README: Usage counts). A detached copy of this tool sends them, so the tool never waits.
    publik_telemetry::start_cli("cadcraft", &publik_version());
    // A copy of this program started to read or write a DWG file does only that.
    if let Some(code) = cadcraft_io::dwg_worker::serve_if_asked() {
        std::process::exit(code);
    }
    cadcraft_io::dwg_worker::use_own_worker();
    // `args_os`, not `args`: a non-Unicode argument (a Latin-1 file name on Linux) must be
    // reported as an error, not panic before anything is printed.
    let code = cadcraft_cli::run(std::env::args_os().skip(1), &mut std::io::stdout(), &mut std::io::stderr());
    std::process::exit(code);
}
