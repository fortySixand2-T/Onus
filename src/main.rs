//! Onus — RTS in Bevy 0.19. Binary entry point; all logic lives in the `onus`
//! library so tests and benches can drive it. See `src/lib.rs`.
//!
//! `cargo run` plays the 1v1 skirmish; `cargo run -- campaign [seed]` opens
//! the campaign client (C6).

fn main() {
    match onus::campaign_client::campaign_args(std::env::args()) {
        Some(seed) => onus::campaign_client::build_campaign_app(seed).run(),
        None => onus::build_app().run(),
    };
}
