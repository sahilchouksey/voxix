//! Service state: shared config, engine, auth verifier, session registry,
//! admission capacity, and the single-permit inference lease (SDK plan §3.3).

use crate::service::api::{SessionResponse, StatusResponse};
use crate::service::auth::AuthFailureObserver;
use crate::service::config::ServiceConfig;
use crate::service::engine::{EngineRuntime, EngineRuntimeStatus, ModelState, StreamTextFrame};
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, OwnedSemaphorePermit, Semaphore};

// ---------------------------------------------------------------------------
// M3: Patchable configuration (PATCH /v1/config live-apply).
// ---------------------------------------------------------------------------

/// The subset of config that can be mutated at runtime via PATCH /v1/config.
/// A separate struct keeps the original `ServiceConfig` immutable (boot-locked
/// fields like `bind` are never exposed here).
#[derive(Debug, Clone)]
pub struct PatchableConfig {
    pub allowed_origins: Vec<String>,
    /// Live cap; admission semaphore stays boot-capped.
    pub max_sessions: u32,
    pub max_audio_seconds: u64,
    pub session_resume_window_ms: u64,
    /// 0 = disabled; otherwise the maximum ms a paused-and-lease-held session
    /// may stay paused before being closed with 4003.
    pub pause_max_ms: u64,
    pub save_history_default: bool,
    pub vad_enabled_default: bool,
    /// Opt-in Opus codec for the v1 audio plane (SDK plan §6; default off).
    /// Live-patchable while the `service-opus` feature is compiled in.
    pub opus_enabled: bool,
}

impl PatchableConfig {
    pub fn from_boot(cfg: &ServiceConfig) -> Self {
        PatchableConfig {
            allowed_origins: vec![],
            max_sessions: cfg.max_sessions,
            max_audio_seconds: cfg.max_audio_seconds,
            session_resume_window_ms: cfg.session_resume_window_ms,
            pause_max_ms: 0,
            save_history_default: true,
            vad_enabled_default: true,
            opus_enabled: cfg.opus_enabled,
        }
    }
}

// ---------------------------------------------------------------------------
// M3: Batch job registry.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchJobState {
    Processing,
    Completed,
    Failed,
    Cancelled,
}

impl BatchJobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            BatchJobState::Processing => "processing",
            BatchJobState::Completed => "completed",
            BatchJobState::Failed => "failed",
            BatchJobState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BatchJob {
    pub job_id: String,
    pub state: BatchJobState,
    pub result: Option<crate::service::api::BatchResult>,
    pub error: Option<crate::service::api::BatchJobError>,
    pub created_at_ms: u64,
}

// ---------------------------------------------------------------------------
// M3: WS ticket (one-use, 60 s, session-bound).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WsTicket {
    pub ticket: String,
    /// Millisecond unix timestamp expiry.
    pub expires_at_ms: u64,
    /// If set, the ticket must be used on the named session's WS upgrade.
    pub session_id: Option<String>,
    /// Set to true after first use.
    pub consumed: bool,
}
/// Terminal sessions remain queryable this long (SDK plan §9: 10 minutes).
pub const SESSION_TERMINAL_RETENTION_MS: u64 = 600_000;

/// Per-session outbound queue capacity (SDK plan §6/§15 S1): stale `partial`
/// coalescing keeps the newest of each turn; the terminal pair (final + its
/// terminal error/close) always fits via reserve before overflow is declared.
pub const OUTBOUND_QUEUE_CAPACITY: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStateName {
    Created,
    Completed,
    Cancelled,
    Failed,
}

impl SessionStateName {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStateName::Created => "created",
            SessionStateName::Completed => "completed",
            SessionStateName::Cancelled => "cancelled",
            SessionStateName::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub session_token: String,
    pub state: SessionStateName,
    pub model_id: Option<String>,
    pub language: String,
    pub created_at_ms: u64,
    pub audio_received_ms: u64,
    pub last_audio_seq: u64,
    pub last_event_seq: u64,
    /// Effective VAD configuration negotiated at create (§3.4).
    pub vad: crate::service::api::VadConfig,
    /// Whether the session wants partial events.
    pub partial_results: bool,
    /// Negotiated audio format for this session (immutable option).
    pub negotiated_audio: crate::service::api::NegotiatedAudio,
    pub final_result: Option<Value>,
    pub error: Option<Value>,
    // M3 resume fields (§15 S2/S3): memory-only, never projected to clients.
    /// Current resume token (rotated on each successful resume). None until
    /// the first `started` event is emitted.
    pub resume_token: Option<String>,
    /// Monotone epoch echoed by the client in `start.resume_epoch`. The server
    /// bumps this on each resume so a stale client fails the echo-check.
    pub resume_epoch: u64,
}

impl SessionState {
    pub fn projection(&self) -> SessionResponse {
        SessionResponse {
            session_id: self.session_id.clone(),
            state: self.state.as_str().to_string(),
            model_id: self.model_id.clone(),
            language: self.language.clone(),
            audio_received_ms: self.audio_received_ms,
            last_audio_seq: self.last_audio_seq,
            last_event_seq: self.last_event_seq,
            created_at: rfc3339_from_ms(self.created_at_ms),
            final_result: self.final_result.clone(),
            error: self.error.clone(),
        }
    }
}

/// RAII bundle for one live session: the single-permit inference lease plus
/// the admission slot. Dropping releases both (cancel/finish/disconnect/panic
/// paths included, since the registry holds the guard).
#[derive(Debug)]
pub struct LeaseGuard {
    inference: Option<OwnedSemaphorePermit>,
    admission: Option<OwnedSemaphorePermit>,
}

impl LeaseGuard {
    pub fn new(inference: OwnedSemaphorePermit, admission: OwnedSemaphorePermit) -> Self {
        LeaseGuard {
            inference: Some(inference),
            admission: Some(admission),
        }
    }
}

/// Registry of held guards, keyed by session id. A session going terminal
/// removes its guard here, releasing the permit via Drop.
#[derive(Debug, Default)]
pub struct LeaseRegistry {
    guards: HashMap<String, LeaseGuard>,
}

impl LeaseRegistry {
    pub fn put(&mut self, id: &str, guard: LeaseGuard) {
        self.guards.insert(id.to_string(), guard);
    }

    pub fn take(&mut self, id: &str) -> Option<LeaseGuard> {
        self.guards.remove(id)
    }

    pub fn held(&self, id: &str) -> bool {
        self.guards.contains_key(id)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn rfc3339_from_secs(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn rfc3339_from_ms(ms: u64) -> String {
    rfc3339_from_secs((ms / 1000) as i64)
}

/// Bearer auth material resolved once at boot: a SHA-256 verifier (daemon
/// mode, read from the 0600 token file) or the minted embedded token.
pub struct AuthContext {
    pub verifier: Option<String>,
    pub embedded_token: Option<String>,
    pub failures: AuthFailureObserver,
}
pub struct ServiceState {
    pub config: ServiceConfig,
    /// Live-patchable config (PATCH /v1/config).
    pub patch_config: Mutex<PatchableConfig>,
    pub engine: Arc<dyn EngineRuntime>,
    pub auth: AuthContext,
    /// Current rotating verifier (replaces `auth.verifier` after the first
    /// POST /v1/auth/tokens?rotate_primary=true; constant-time compared).
    pub rotating_verifier: Mutex<Option<String>>,
    /// Current auth epoch: incremented on token rotation. WS tasks subscribe
    /// to the broadcast channel and close 4001 when the epoch advances past
    /// the value they saw at upgrade.
    pub auth_epoch: Mutex<u64>,
    /// Broadcast to WS tasks when a rotation occurs.
    pub auth_epoch_tx: broadcast::Sender<u64>,
    /// Broadcast bus for engine stream-text partials → active WS sessions
    /// (§6 `partial`; service owns the single bus, sessions attribute).
    pub stream_text: broadcast::Sender<StreamTextFrame>,
    /// Nonterminal session registry.
    pub live: Mutex<HashMap<String, SessionState>>,
    /// Terminal sessions retained for the retention window.
    pub terminal: Mutex<HashMap<String, SessionState>>,
    /// The single-permit inference lease.
    pub inference: Arc<Semaphore>,
    /// Admission capacity (max concurrent live sessions).
    pub admission: Arc<Semaphore>,
    /// Held lease guards keyed by session.
    pub leases: Mutex<LeaseRegistry>,
    /// Sessions with a live WS socket bound to them (single-socket contract).
    pub live_streams: Mutex<HashSet<String>>,
    /// Client-session idempotency: key → (session_id, created_at_ms).
    pub client_sessions: Mutex<HashMap<String, (String, u64)>>,
    /// Fires when a session transitions terminal via REST (cancel/finish) so
    /// an in-flight WS task can observe and tear down without re-terminalizing.
    pub cancelled_notifications: broadcast::Sender<String>,
    /// Compare-and-swap slot for model preparation (§15 G7 single-flight):
    /// exactly one prepare runs at a time; a concurrent CAS refusal answers
    /// 409 `prepare-busy`. Held for the whole prepare including the SSE tail.
    pub prepare_slot: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// Live prepare stages keyed by model id (polling fallback projection).
    pub prepares:
        std::sync::Mutex<std::collections::HashMap<String, crate::service::api::PrepareStage>>,
    /// M3: The command slot shared between prepare and batch admission. A
    /// concurrent batch → prepare returns 409 prepare-busy; concurrent batch
    /// → batch returns 429 queue-full.
    pub command_slot: std::sync::Arc<tokio::sync::Mutex<()>>,
    /// M3: Batch job registry keyed by job_id.
    pub jobs: Mutex<HashMap<String, BatchJob>>,
    /// M3: One-use WS tickets keyed by ticket string.
    pub ws_tickets: Mutex<HashMap<String, WsTicket>>,
    /// M3: Service-owned history database (optional; None if init failed).
    pub service_history: Option<Arc<crate::service::history::ServiceHistory>>,
    pub started_at_ms: u64,
}

impl ServiceState {
    pub fn new(
        config: ServiceConfig,
        engine: Arc<dyn EngineRuntime>,
        verifier: Option<String>,
        embedded_token: Option<String>,
        stream_text: broadcast::Sender<StreamTextFrame>,
    ) -> Self {
        let patch_config = PatchableConfig::from_boot(&config);
        let (auth_epoch_tx, _) = broadcast::channel(32);
        let command_slot = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        // History init is best-effort; a failure logs but never aborts startup.
        let history_dir = config.data_dir.join("state");
        let service_history = crate::service::history::ServiceHistory::open(&history_dir)
            .map_err(|e| log::warn!(target: "service::state", "history init failed: {e}"))
            .ok()
            .map(std::sync::Arc::new);
        ServiceState {
            config: config.clone(),
            patch_config: Mutex::new(patch_config),
            engine,
            auth: AuthContext {
                verifier,
                embedded_token,
                failures: AuthFailureObserver::logging(),
            },
            rotating_verifier: Mutex::new(None),
            auth_epoch: Mutex::new(0),
            auth_epoch_tx,
            stream_text,
            live: Mutex::new(HashMap::new()),
            terminal: Mutex::new(HashMap::new()),
            inference: Arc::new(Semaphore::new(1)),
            admission: Arc::new(Semaphore::new(config.max_sessions as usize)),
            leases: Mutex::new(LeaseRegistry::default()),
            live_streams: Mutex::new(HashSet::new()),
            client_sessions: Mutex::new(HashMap::new()),
            cancelled_notifications: broadcast::channel(64).0,
            prepare_slot: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            prepares: std::sync::Mutex::new(std::collections::HashMap::new()),
            command_slot,
            jobs: Mutex::new(HashMap::new()),
            ws_tickets: Mutex::new(HashMap::new()),
            service_history,
            started_at_ms: now_ms(),
        }
    }

    /// The currently recorded prepare stage for a model (the prepare slot is
    /// a CAS primitive; this map carries the polling projection). A model
    /// never prepared reports `not_started` — nothing was queued.
    pub fn prepare_stage(&self, model_id: &str) -> crate::service::api::PrepareStage {
        self.prepares
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(model_id)
            .cloned()
            .unwrap_or(crate::service::api::PrepareStage::NotStarted)
    }

    pub fn set_prepare_stage(
        &self,
        model_id: &str,
        stage: crate::service::api::PrepareStage,
    ) -> Result<(), String> {
        let mut stages = self.prepares.lock().unwrap_or_else(|e| e.into_inner());
        stages.insert(model_id.to_string(), stage);
        Ok(())
    }

    /// Convenience setter for the failed terminal stage.
    pub fn set_prepare_stage_error(&self, model_id: &str, message: &str) -> Result<(), String> {
        self.set_prepare_stage(
            model_id,
            crate::service::api::PrepareStage::Failed(message.to_string()),
        )
    }

    /// Constant-time bearer verification over digests. Missing header →
    /// false (and one (throttled) failure observation).
    pub fn verify_bearer(&self, presented: Option<&str>) -> bool {
        let Some(presented) = presented else {
            self.auth.failures.record_failure();
            return false;
        };
        let Some(verifier) = &self.auth.verifier else {
            self.auth.failures.record_failure();
            return false;
        };
        let digest = crate::service::auth::sha256_digest(presented);
        let ok = bool::from(digest.as_bytes().ct_eq(verifier.as_bytes()));
        if !ok {
            self.auth.failures.record_failure();
        }
        ok
    }

    /// Session-token sub-credential check. Wrong or missing token behaves
    /// identically to an unknown id, so ids cannot be probed (§15 G9/G14).
    pub fn verify_session_token(&self, session_id: &str, presented: Option<&str>) -> bool {
        let Some(token) = presented else {
            return false;
        };
        self.find_session(session_id)
            .map(|s| s.session_token == token)
            .unwrap_or(false)
    }

    fn find_in(
        &self,
        map: &Mutex<HashMap<String, SessionState>>,
        id: &str,
    ) -> Option<SessionState> {
        map.lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn find_session(&self, id: &str) -> Option<SessionState> {
        self.find_in(&self.live, id)
            .or_else(|| self.find_in(&self.terminal, id))
    }

    pub fn live_count(&self) -> u32 {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).len() as u32
    }

    /// Register a freshly created live session holding a lease guard.
    pub fn insert_live(&self, session: SessionState, guard: LeaseGuard) {
        let id = session.session_id.clone();
        self.leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(&id, guard);
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, session);
    }

    /// Move a live session to the terminal map (state + retention window),
    /// dropping its lease guard so the single-permit inference lease releases.
    pub fn set_terminal(
        &self,
        id: &str,
        state: SessionStateName,
        final_result: Option<Value>,
        error: Option<Value>,
    ) -> Option<SessionState> {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        let session = live.remove(id).map(|mut s| {
            s.state = state;
            s.final_result = final_result.clone();
            s.error = error.clone();
            s
        });
        drop(live);
        let session = session?;
        self.leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take(&session.session_id);
        self.terminal
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), session.clone());
        Some(session)
    }

    /// Notify attached WS tasks that a session transitioned terminal via an
    /// out-of-band REST call (cancel/finish).
    pub fn mark_cancelled(&self, id: &str) {
        let _ = self.cancelled_notifications.send(id.to_string());
    }

    /// Engine status projection + service-level fields (§15 S6/S8).
    pub fn status(&self) -> StatusResponse {
        let st: EngineRuntimeStatus = EngineRuntime::status(&*self.engine);
        let lease_holder = self
            .live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .next()
            .cloned();
        StatusResponse {
            mode: self.config.mode.as_str().into(),
            bind: self.config.bind.clone(),
            active_model_id: st.selected_model_id.clone(),
            model_state: st.model_state.as_str().to_string(),
            backend: st.backend.clone(),
            active_session_id: lease_holder,
            active_sessions: self.live_count(),
            max_realtime_factor: st.max_realtime_factor,
            lease_held: self.inference.available_permits() == 0,
            capabilities: self.engine.capabilities(),
        }
    }

    // ------------------------------------------------------------------
    // M3: auth epoch / rotation helpers.
    // ------------------------------------------------------------------

    /// Rotate the primary bearer token: store the new verifier, bump the epoch,
    /// and broadcast so WS tasks arm 4001. Returns the new epoch.
    pub fn rotate_token(&self, new_verifier: String) -> u64 {
        let epoch = {
            let mut e = self.auth_epoch.lock().unwrap_or_else(|e| e.into_inner());
            *e += 1;
            *e
        };
        {
            let mut cell = self
                .rotating_verifier
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *cell = Some(new_verifier);
        }
        let _ = self.auth_epoch_tx.send(epoch);
        epoch
    }

    /// Constant-time bearer check that also consults the rotating verifier.
    pub fn verify_bearer_rotating(&self, presented: Option<&str>) -> bool {
        let Some(presented) = presented else {
            self.auth.failures.record_failure();
            return false;
        };
        let digest = crate::service::auth::sha256_digest(presented);
        // Check the current rotating verifier first.
        {
            let cell = self
                .rotating_verifier
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(v) = cell.as_deref() {
                if bool::from(digest.as_bytes().ct_eq(v.as_bytes())) {
                    return true;
                }
            }
        }
        // Fall back to the boot-time verifier (only if no rotating one set yet).
        let has_rotating = self
            .rotating_verifier
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if !has_rotating {
            if let Some(v) = &self.auth.verifier {
                if bool::from(digest.as_bytes().ct_eq(v.as_bytes())) {
                    return true;
                }
            }
        }
        // Client sidecar (GUI etc.) — checked last, only on a miss.
        let state_dir = self.config.data_dir.join("state");
        if crate::service::auth::verify_against_clients(&state_dir, presented) {
            return true;
        }
        self.auth.failures.record_failure();
        false
    }

    // ------------------------------------------------------------------
    // M3: WS ticket helpers.
    // ------------------------------------------------------------------

    /// Mint and register a one-use ticket. Returns `(ticket_string, expires_at_rfc3339)`.
    pub fn mint_ticket(&self, session_id: Option<String>) -> (String, String) {
        let ticket = crate::service::auth::generate_token()
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
        let expires_at_ms = now_ms() + 60_000;
        let entry = WsTicket {
            ticket: ticket.clone(),
            expires_at_ms,
            session_id,
            consumed: false,
        };
        self.ws_tickets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ticket.clone(), entry);
        let expires_str = rfc3339_from_secs((expires_at_ms / 1000) as i64);
        (ticket, expires_str)
    }

    /// Validate and consume a one-use ticket for the given session upgrade.
    /// Returns true if the ticket is valid for `session_id`.
    pub fn consume_ticket(&self, ticket: &str, session_id: &str) -> bool {
        let mut map = self.ws_tickets.lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = map.get_mut(ticket) else {
            return false;
        };
        if entry.consumed {
            map.remove(ticket);
            return false;
        }
        if now_ms() > entry.expires_at_ms {
            map.remove(ticket);
            return false;
        }
        // Session-binding check: if bound, must match.
        if let Some(ref bound_sid) = entry.session_id.clone() {
            if bound_sid != session_id {
                return false;
            }
        }
        entry.consumed = true;
        map.remove(ticket);
        true
    }

    // ------------------------------------------------------------------
    // M3: batch job registry.
    // ------------------------------------------------------------------

    pub fn register_job(&self, job: BatchJob) {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job.job_id.clone(), job);
    }

    pub fn get_job(&self, job_id: &str) -> Option<BatchJob> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(job_id)
            .cloned()
    }

    pub fn update_job(&self, job: BatchJob) {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job.job_id.clone(), job);
    }

    pub fn cancel_job(&self, job_id: &str) -> bool {
        let mut map = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = map.get_mut(job_id) {
            if job.state == BatchJobState::Processing {
                job.state = BatchJobState::Cancelled;
                return true;
            }
        }
        false
    }

    // ------------------------------------------------------------------
    // M3: resume token helpers.
    // ------------------------------------------------------------------

    /// Update the resume token + epoch for a live session. Called when the
    /// `started` event is emitted (initial or on resume).
    pub fn set_resume_token(&self, session_id: &str, token: String, epoch: u64) {
        let mut live = self.live.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = live.get_mut(session_id) {
            s.resume_token = Some(token);
            s.resume_epoch = epoch;
        }
    }

    /// Read the resume creds for a live session (for epoch echo-check).
    pub fn resume_creds(&self, session_id: &str) -> Option<(String, u64)> {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .and_then(|s| s.resume_token.clone().map(|t| (t, s.resume_epoch)))
    }

    /// Evict expired tickets (called periodically; errors are ignored).
    pub fn evict_expired_tickets(&self) {
        let now = now_ms();
        let mut map = self.ws_tickets.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, v| !v.consumed && v.expires_at_ms > now);
    }
}

impl ModelState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ModelState::Unloaded => "unloaded",
            ModelState::Loading => "loading",
            ModelState::Loaded => "loaded",
        }
    }
}

pub fn session_expiry(resume_window_ms: u64) -> String {
    let add = resume_window_ms / 1000;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_from_secs(now + add.max(1) as i64)
}
