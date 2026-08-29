//! Onus — RTS in Bevy 0.19. Binary entry point; all logic lives in the `onus`
//! library so tests and benches can drive it. See `src/lib.rs`.

fn main() {
    onus::build_app().run();
}
