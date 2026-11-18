//! Integration test for the multi-client sidecar (state/clients.json).
//! The GUI registers an independent bearer here so any client (GUI, phone,
//! SDK) authenticates against the same service.

use voxix_app_lib::service::auth::{ensure_gui_client_token, verify_against_clients};

#[test]
fn gui_client_registers_and_verifies() {
    let dir = std::env::temp_dir().join(format!(
        "voxix-clients-test-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let state_dir = dir.join("state");

    let token1 = ensure_gui_client_token(&state_dir).expect("mint failed");
    assert!(token1.starts_with("vx_"));

    let token2 = ensure_gui_client_token(&state_dir).expect("re-read failed");
    assert_eq!(token1, token2);

    assert!(verify_against_clients(&state_dir, &token1));
    assert!(!verify_against_clients(
        &state_dir,
        "vx_wrongtokenwrongtokenwrongtoken"
    ));
    assert!(!verify_against_clients(&state_dir, ""));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let clients = state_dir.join("clients.json");
        let gui = state_dir.join("gui-client.json");
        let meta = std::fs::metadata(&clients).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let meta = std::fs::metadata(&gui).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

    let clients = std::fs::read_to_string(state_dir.join("clients.json")).unwrap();
    assert!(!clients.contains(&token1), "clients.json leaked plaintext");
}
