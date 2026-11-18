# voxix-keys

Cross-platform global keyboard shortcuts library for Rust, vendored into this
repository (path dependency).

## Features

- **Cross-platform**: Works on macOS, Windows, and Linux
- **Global hotkeys**: Register system-wide keyboard shortcuts
- **Hotkey blocking**: Registered hotkeys are blocked from reaching other applications
- **Modifier-only hotkeys**: Support for shortcuts like `Cmd+Shift` without a key
- **String parsing**: Parse hotkeys from strings like `"Ctrl+Alt+Space"`
- **Hotkey recording**: Low-level keyboard listener for "record a hotkey" UI flows
- **Serde support**: All types implement `Serialize`/`Deserialize`

## Vendoring

The source is a scrubbed rename of the pinned upstream release of this crate,
version `0.3.3` (registry checksum
`71f78470c4a23919ba16e6d621fa2c429451faa98b073f4cba679c496b3e8fa2`), vendored
locally per repository policy. Public API is unchanged except for the
neutralized naming.

## Quick Start

```rust
use voxix_keys::{HotkeyManager, Hotkey, Modifiers, Key};

fn main() -> Result<(), voxix_keys::Error> {
    let mut manager = HotkeyManager::new()?;

    // Register Cmd+Shift+K on macOS (Ctrl+Shift+K on Windows/Linux):
    // the Hotkey type handles the platform differences for you.
    manager.register(Hotkey::new(Modifiers::CMD | Modifiers::SHIFT, Key::K)?, || {
        println!("Hotkey triggered!");
    })?;

    manager.listen_forever();

    Ok(())
}
```

## Documentation

For a detailed API reference, please explore `src/lib.rs` and the types
(`types/*`) — crate-level doc examples cover registration, blocking,
recording, and accessibility checks.

## License

MIT — see `LICENSE`.
