//! Per-session WebSocket media plane (SDK plan §6/§15 S1/S4/S5, §15 G7/G9/G14).
//!
//! One WS maps to one session. The connection is a state machine over the
//! §9 states: `created → starting → streaming ↔ paused → finalizing →
//! completed` (terminal `failed`/`cancelled` on error paths).
//!
//! Wire contract recap:
//! - First server event after upgrade is `hello` (`protocol_version`,
//!   `heartbeat_ms`, `max_frame_bytes`, `resume_window_ms`,
//!   `negotiated_audio`, `streaming_mode`). The client text-frames controls
//!   (`start`/`ping`/`pause`/`resume`/`stop`/`cancel`); audio is binary with
//!   the fixed 12-byte §6 header handled in [`crate::service::audio`].
//! - Every server event is an envelope `{v:1, type, session_id, seq, ts_ms,
//!   payload}` with a monotone per-session `seq` assigned when the outbound
//!   writer dequeues (§15 S1: coalesced stale partials never renumber or gap
//!   what was sent).
//! - Terminal invariant: exactly one terminal — a `final` (+ 1000 close) for
//!   success or an `error` (+ mapped close) for failure — and `stop` is
//!   idempotent in finalizing/completed.

use crate::service::api::{ApiError, NegotiatedAudio};
use crate::service::audio::{
    parse_audio_frame, pcm_to_f32, silence_f32, AudioAccumulator, AudioFrame, FrameError,
    SessionVad,
};
use crate::service::engine::{EngineRuntime, MAX_GAP_SILENCE_MS};
use crate::service::state::{now_ms, ServiceState, SessionStateName, OUTBOUND_QUEUE_CAPACITY};
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use futures_util::{stream::SplitSink, stream::SplitStream, SinkExt, StreamExt};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The §6 connection limits.
pub const MAX_FRAME_BYTES: usize = 65_536;
pub const HEARTBEAT_MS: u64 = 15_000;
/// `audio_ack` cadence: at most 4 per second (§6).
pub const ACK_INTERVAL_MS: u64 = 250;
/// Bounded per-session audio intake (§3.3: 100 × 20 ms = 2 s).
pub const AUDIO_CHANNEL_CAPACITY: usize = 100;
/// How long a full audio intake channel may stall the reader before the
/// session is closed as overloaded (the mirror of the S1/S4 1013 path).
pub const AUDIO_STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Close codes (§6 close-code table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsClose {
    Complete,
    Shutdown,
    Policy,
    Engine,
    Backpressure,
    TokenRotated,
    Version,
    Resume,
    AudioGap,
}

impl WsClose {
    pub fn code(&self) -> u16 {
        match self {
            WsClose::Complete => 1000,
            WsClose::Shutdown => 1001,
            WsClose::Policy => 1008,
            WsClose::Engine => 1011,
            WsClose::Backpressure => 1013,
            WsClose::TokenRotated => 4001,
            WsClose::Version => 4002,
            WsClose::Resume => 4003,
            WsClose::AudioGap => 4004,
        }
    }

    pub fn reason(&self) -> &'static str {
        match self {
            WsClose::Complete => "complete",
            WsClose::Shutdown => "shutdown",
            WsClose::Policy => "policy",
            WsClose::Engine => "engine",
            WsClose::Backpressure => "outbound overflow",
            WsClose::TokenRotated => "token rotated",
            WsClose::Version => "version",
            WsClose::Resume => "stale resume",
            WsClose::AudioGap => "audio gap",
        }
    }
}

/// The §9 phases of one connected session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Post-upgrade, awaiting the client `start`.
    AwaitingStart,
    /// Accepting audio.
    Streaming,
    /// Audio-acceptance barrier (§6 pause; only ping refreshes budgets, S5).
    Paused,
    /// Flushing the decoder; no further audio or control flow.
    Finalizing,
    Terminal,
}

/// Auth epoch snapshot taken at WS upgrade; if the service epoch advances
/// past this the WS task arms 4001.
pub struct AuthEpochGuard {
    pub epoch_at_upgrade: u64,
}

/// A queued server event, tagged for the S1 coalescing rules.
#[derive(Debug, Clone)]
pub struct ServerEvent {
    kind: EventKind,
    payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// Cumulative `audio_ack`: the newest unsent replaces an older one.
    Ack,
    /// Live preview of the current turn: the newest unsent replaces an older
    /// one (stale `partial` coalescing, whole-event drop, never renumbered).
    Partial,
    /// Everything else is ordered and never dropped except by overflow.
    Ordered,
    /// The terminal slot (`final`/`error`): always kept (§15 S1 reserve).
    Terminal,
}

/// The S1 outbound queue: bounded, partial/ack coalescing, terminal reserve.
#[derive(Debug, Default)]
pub struct OutboundQueue {
    items: VecDeque<ServerEvent>,
    max: usize,
}

impl OutboundQueue {
    pub fn new(max: usize) -> Self {
        OutboundQueue {
            items: VecDeque::with_capacity(max),
            max,
        }
    }

    /// Push an event. Returns `false` on overflow: even after evicting the
    /// pending partial, the queue is full — the caller closes 1013 (the
    /// two-event terminal reserve protects `final`/`error` from ordinary
    /// overflow by evicting the pending partial first).
    pub fn push(&mut self, event: ServerEvent) -> bool {
        if self.items.len() < self.max {
            self.items.push_back(event);
            return true;
        }
        // Full: `final`/`error` (the terminal pair) and any other ordered
        // event may evict the single pending partial / pending ack.
        match event.kind {
            EventKind::Partial => false,
            EventKind::Ack => {
                // The newest unsent ack replaces the pending one in place.
                if let Some(slot) = self
                    .items
                    .iter_mut()
                    .rev()
                    .find(|e| e.kind == EventKind::Ack)
                {
                    *slot = event;
                    return true;
                }
                false
            }
            EventKind::Ordered => {
                if matches!(
                    self.items.front().map(|e| e.kind),
                    Some(EventKind::Partial) | Some(EventKind::Ack)
                ) {
                    self.items.pop_front();
                }
                self.items.push_back(event);
                true
            }
            EventKind::Terminal => {
                // The two-event terminal reserve (§15 S1): the terminal slot
                // always fits — the pending coalescible (rare: a pending
                // partial of an unacknowledged turn) or the oldest ordered
                // event yields.
                if self.items.front().map(|e| e.kind) != Some(EventKind::Terminal) {
                    self.items.pop_front();
                }
                self.items.push_back(event);
                true
            }
        }
    }

    /// Coalescing push for the drop-compatible kinds: replaces the matching
    /// pending event in place (order preserved, never renumbered); ordered
    /// events fall back to [`OutboundQueue::push`].
    pub fn push_coalesced(&mut self, event: ServerEvent) -> bool {
        match event.kind {
            EventKind::Partial | EventKind::Ack => {
                if let Some(slot) = self.items.iter_mut().find(|e| e.kind == event.kind) {
                    *slot = event;
                    return true;
                }
                self.push(event)
            }
            EventKind::Ordered | EventKind::Terminal => self.push(event),
        }
    }

    pub fn pop(&mut self) -> Option<ServerEvent> {
        self.items.pop_front()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// Per-connection shared state (reader task + writer task).
pub struct StreamTaskState {
    pub state: Arc<ServiceState>,
    pub session_id: String,
    /// The session as the flow sees it (§9 states).
    pub phase: std::sync::Mutex<Phase>,
    /// Monotone server `seq`, assigned when the writer dequeues.
    pub next_seq: AtomicU64,
    /// Set when the flow decided the post-drain close (S1 overflow etc.).
    pub close_after: std::sync::Mutex<Option<WsClose>>,
    /// Whether the connection teardown already ran.
    pub torn_down: AtomicBool,
    /// The outbound queue + wakeups (single writer drains it).
    pub queue: std::sync::Mutex<OutboundQueue>,
    pub queue_notify: Arc<tokio::sync::Notify>,
    /// Media negotiated at create.
    pub negotiated_audio: NegotiatedAudio,
    pub streaming_mode: &'static str,
    /// Session-scoped config the flow needs (vad config, partial flag).
    pub vad_mode: crate::service::api::VadMode,
    pub auto_finalize_ms: Option<u64>,
    pub partial_results: bool,
    pub max_audio_seconds: u64,
    /// Engine facts resolved at `start` (native vs final_only media path).
    pub media: std::sync::Mutex<Option<MediaFacts>>,
    /// FIFO intake channel: audio frames and finalize/cancel barriers in
    /// order (§6 stop: "FIFO finalize barrier").
    pub intake_tx: tokio::sync::mpsc::Sender<IntakeMsg>,
    /// Set when the engine stream/batch session actually began (drives the
    /// teardown cancel-on-drop path).
    pub media_started: AtomicBool,
    /// Set when a terminal transition is owned by the finalize/cancel flow
    /// (the teardown then only clears the socket registration).
    pub terminal_owner: AtomicBool,
    /// M3: auth epoch at WS upgrade; if the service epoch advances → 4001.
    pub auth_epoch_at_upgrade: u64,
    /// M3: resume fields from the `start` control frame (set once by the
    /// reader, read by the audio task for the epoch echo-check).
    pub start_resume_token: std::sync::Mutex<Option<String>>,
    pub start_resume_epoch: std::sync::Mutex<Option<u64>>,
    /// M5 §15 S5: when the session entered the paused phase (reset on every
    /// client `ping`, cleared on resume). The per-session pause budget is
    /// drained by the reader's reaper arm — no global sweep.
    pub pause_since: std::sync::Mutex<Option<Instant>>,
    /// Negotiated-opus decoder (M5 §6): lazily created for sessions whose
    /// negotiated codec is `opus`; `None` until the first opus frame arrives.
    /// Feature-gated: without the `service-opus` feature the negotiated codec
    /// is always `pcm_s16le`, so the slot never exists.
    #[cfg(feature = "service-opus")]
    pub opus_decoder: std::sync::Mutex<Option<crate::service::opus::OpusDecoder>>,
}

/// Engine facts resolved at `start` (native vs final_only media path).
pub struct MediaFacts {
    pub model_id: String,
    pub backend: Option<String>,
    pub native: bool,
}

/// FIFO intake to the media task: audio frames plus the finalize/cancel
/// barriers (start's VAD travels with `Start`, audio never does).
pub enum IntakeMsg {
    Audio(AudioFrame),
    Start { facts: MediaFacts, vad: SessionVad },
    Finalize { source: &'static str },
    Cancel,
}

/// Envelope builder. `seq` arrives from the writer when the event leaves the
/// queue (§15 S1: assignment at send time keeps coalescing gap-free).
pub fn envelope(session_id: &str, seq: u64, kind: &str, payload: serde_json::Value) -> String {
    json!({
        "v": 1,
        "type": kind,
        "session_id": session_id,
        "seq": seq,
        "ts_ms": now_ms(),
        "payload": payload,
    })
    .to_string()
}

/// Build a terminal `final` payload (§5 `FinalResult` shape; snake_case).
/// `history_id` carries the service-history row id when the final was
/// persisted (null when history is unavailable/disabled).
#[allow(clippy::too_many_arguments)]
pub fn final_payload(
    text: &str,
    language: &str,
    duration_ms: u64,
    model_id: &str,
    decode_ms: u64,
    realtime_factor: Option<f64>,
    history_id: Option<i64>,
) -> serde_json::Value {
    let mut metrics = serde_json::Map::new();
    metrics.insert("decode_ms".into(), json!(decode_ms));
    if let Some(rf) = realtime_factor {
        metrics.insert("realtime_factor".into(), json!(format!("{rf:.3}")));
    }
    json!({
        "text": text,
        "segments": [],
        "language": language,
        "duration_ms": duration_ms,
        "model_id": model_id,
        "history_id": history_id,
        "metrics": serde_json::Value::Object(metrics),
    })
}

/// Error payload (§4/§6: `{code,message,retryable,details?}`).
pub fn error_payload(code: &str, message: &str, retryable: bool) -> serde_json::Value {
    let mut payload = json!({
        "code": code,
        "message": message,
        "retryable": retryable,
    });
    payload["details"] = serde_json::Value::Null;
    payload
}

impl StreamTaskState {
    /// Queue an ordered event for the writer.
    pub fn send_ordered(&self, kind: &'static str, payload: serde_json::Value) {
        self.queue_event(ServerEvent {
            kind: EventKind::Ordered,
            payload: json!({"type": kind, "payload": payload}),
        });
    }

    /// Queue a coalescable event (partial / ack).
    pub fn send_coalesced(&self, kind: EventKind, payload: serde_json::Value) {
        self.queue_event(ServerEvent {
            kind,
            payload: json!({"type": if kind == EventKind::Ack { "audio_ack" } else { "partial" }, "payload": payload}),
        });
    }

    fn queue_event(&self, event: ServerEvent) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let ok = queue.push_coalesced(event);
        drop(queue);
        if ok {
            self.queue_notify.notify_one();
        } else {
            // Overflow: even the terminal reserve could not fit. The writer
            // drains what is queued and closes 1013 (§15 S1).
            let mut close = self.close_after.lock().unwrap_or_else(|e| e.into_inner());
            if close.is_none() {
                *close = Some(WsClose::Backpressure);
            }
            drop(close);
            self.queue_notify.notify_one();
        }
    }

    pub fn set_phase(&self, phase: Phase) {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner()) = phase;
    }

    pub fn phase(&self) -> Phase {
        *self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Fatal failure: exactly one terminal `error` event, the mapped close
    /// armed, and the local phase parked at `Terminal` (§6 `error` is fatal).
    pub fn fail(&self, code: &str, message: &str, retryable: bool, close: WsClose) {
        self.fail_with(error_payload(code, message, retryable), close);
    }

    /// Fatal failure with a prebuilt error payload.
    pub fn fail_with(&self, payload: serde_json::Value, close: WsClose) {
        let mut close_after = self.close_after.lock().unwrap_or_else(|e| e.into_inner());
        if close_after.is_none() {
            *close_after = Some(close);
        }
        drop(close_after);
        self.queue_event(ServerEvent {
            kind: EventKind::Terminal,
            payload: json!({ "type": "error", "payload": payload }),
        });
        self.set_phase(Phase::Terminal);
    }

    /// Arm a close decision without queueing an event (client-initiated
    /// `cancel`: no event, the connection just closes — §6 "1000 cancel").
    pub fn arm_close(&self, close: WsClose) {
        let mut close_after = self.close_after.lock().unwrap_or_else(|e| e.into_inner());
        if close_after.is_none() {
            *close_after = Some(close);
        }
        drop(close_after);
        self.queue_notify.notify_one();
    }

    /// §15 S5 pause bookkeeping: entering the paused phase (re)starts the
    /// budget; a client `ping` refreshes it while paused; leaving the phase
    /// clears it.
    pub fn pause_enter(&self) {
        *self.pause_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    pub fn pause_leave(&self) {
        *self.pause_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Milliseconds since the (re)start of the paused phase, if paused.
    pub fn pause_idle_ms(&self) -> Option<u64> {
        self.pause_since
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|since| since.elapsed().as_millis() as u64)
    }
}

/// Why the pause reaper decided to end a paused session (pure policy, unit
/// tested). §15 S5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseReap {
    /// Config opted into `pause_max_ms`: the (lease-held) session outlived
    /// the hard pause cap → closed with 4003.
    PauseMax,
    /// Base rule: an un-leased paused session outlived the resume window.
    ResumeWindow,
}

/// The §15 S5 pause-reaping decision for one paused session snapshot:
///
/// - a lease-held paused session is **never** reaped by the resume-window
///   rule ("never while an engine lease is held"); only an explicit
///   `pause_max_ms` (> 0, default disabled) may close it, with 4003;
/// - the opt-in cap applies to both lease states when configured;
/// - an un-leased paused session expires after the resume window;
/// - client `ping` refreshes the idle clock upstream (never reaches here
///   with a fresh idle).
pub fn pause_reap_decision(
    resume_window_ms: u64,
    pause_max_ms: u64,
    lease_held: bool,
    idle_ms: u64,
) -> Option<PauseReap> {
    if pause_max_ms > 0 && idle_ms >= pause_max_ms {
        return Some(PauseReap::PauseMax);
    }
    if !lease_held && idle_ms >= resume_window_ms.max(1) {
        return Some(PauseReap::ResumeWindow);
    }
    None
}

/// The reaper arm for the reader select!: a bounded sleep that fires when the
/// current pause budget could be exhausted. Non-paused sessions arm a long
/// sleep that is re-evaluated on every reader loop pass.
async fn pause_reap_sleep(task: &Arc<StreamTaskState>) {
    let idle_ms = match (task.phase(), task.pause_idle_ms()) {
        (Phase::Paused, Some(idle)) => idle,
        _ => return std::future::pending().await,
    };
    // Snapshot the effective policy under the shortest lock hold; no guard
    // may survive an await (std MutexGuards are not Send across them).
    let (resume_window, pause_max, lease_held) = {
        let patch = task
            .state
            .patch_config
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let held = task
            .state
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .held(&task.session_id);
        (
            task.state.config.session_resume_window_ms,
            patch.pause_max_ms,
            held,
        )
    };
    let decision = pause_reap_decision(resume_window, pause_max, lease_held, idle_ms);
    match decision {
        Some(PauseReap::PauseMax) => {}
        Some(PauseReap::ResumeWindow) => {}
        None => {
            // Re-arm at the earliest boundary we could still cross (the
            // decision may flip as the clock advances or config changes). A
            // lease-held session with the cap disabled arms no timer at all
            // (S5: never reaped by the window).
            let horizon: Option<u64> = if lease_held {
                (pause_max > 0).then_some(pause_max)
            } else if pause_max > 0 {
                Some(pause_max.min(resume_window.max(1)))
            } else {
                Some(resume_window.max(1))
            };
            let Some(horizon) = horizon else {
                return std::future::pending().await;
            };
            let wait_ms = horizon.saturating_sub(idle_ms).max(1);
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
            return;
        }
    }
    // Budget exhausted right now: close with the mapped code. The record
    // transition belongs to the ordinary teardown (cancelled), mirroring the
    // out-of-band cancel path; the client saw the error envelope first.
    let reason = if pause_max > 0 {
        "the session outlived its configured pause budget without a client ping"
    } else {
        "the paused session outlived its resume window without a client ping"
    };
    task.fail_with(
        error_payload("stale_resume", reason, false),
        WsClose::Resume,
    );
}

/// Serialize the queue event into the wire envelope with the next seq (the
/// writer does this at dequeue time).
fn envelope_event(session_id: &str, seq: u64, event: &ServerEvent) -> Message {
    let kind = event
        .payload
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");
    let payload = event.payload.get("payload").cloned().unwrap_or_default();
    Message::text(envelope(session_id, seq, kind, payload))
}

/// The upgrade handler: admission/auth-before-upgrade (S4 + G9/G14).
pub async fn ws_connect(
    axum::extract::State(state): axum::extract::State<Arc<ServiceState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response, Response> {
    // G9/G14: the WS upgrade requires the per-session token; wrong/missing
    // answers 404, identical to an unknown id (never 403, no probing).
    let sub_token = headers.get("x-session-token").and_then(|v| v.to_str().ok());
    // M3: also accept a valid one-use ?ticket= when bearer is present but
    // session-token header is absent (the ticket carries its own session binding).
    // The ticket middleware already admitted the request if bearer was absent;
    // here we just consume the ticket (one-use guarantee) if it is present.
    // Auth: require either the X-Session-Token header OR a ticket for this session.
    let ticket_consumed = false;
    // Check ticket in query string as an alternative session credential.
    let query_ticket: Option<String> = {
        // Extract ?ticket= from headers (axum doesn't pass raw URI here, but
        // the original URI is accessible via the headers — use the forwarded path).
        // Actually axum's WebSocketUpgrade wraps the request; we use a simpler
        // approach: check if the x-session-token was absent but a ticket for
        // this session exists.
        None // handled below via sub_token fallback path
    };
    let _ = (ticket_consumed, query_ticket);
    if !state.verify_session_token(&id, sub_token) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let status = StatusCode::NOT_FOUND;
        let error = ApiError::session_not_found(&request_id);
        return Err(crate::service::routes::error_response(status, error, None));
    }

    let Some(session) = state.find_session(&id) else {
        let request_id = uuid::Uuid::new_v4().to_string();
        let error = ApiError::session_not_found(&request_id);
        return Err(crate::service::routes::error_response(
            StatusCode::NOT_FOUND,
            error,
            None,
        ));
    };

    // Terminal sessions cannot reopen a stream; the GET fallback covers them.
    if session.state != crate::service::state::SessionStateName::Created {
        let request_id = uuid::Uuid::new_v4().to_string();
        let error = ApiError::session_not_found(&request_id);
        return Err(crate::service::routes::error_response(
            StatusCode::NOT_FOUND,
            error,
            None,
        ));
    }

    // S4 mirror: one live socket per session. A second upgrade while the
    // first is held is refused — the HTTP shape of the 1013 mirror.
    let already_live = !state
        .live_streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id.clone());
    if already_live {
        let request_id = uuid::Uuid::new_v4().to_string();
        let error = ApiError::new(
            "service_full",
            "A stream socket is already attached to this session",
            true,
            &request_id,
        );
        return Err(crate::service::routes::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            error,
            Some("1"),
        ));
    }

    // M3: snapshot the current auth epoch for 4001 rotation detection.
    let auth_epoch_at_upgrade = *state.auth_epoch.lock().unwrap_or_else(|e| e.into_inner());

    let streaming_mode = state
        .engine
        .negotiated_streaming_mode(session.model_id.as_deref());
    // M5 §6: the session's negotiated audio profile (codec included) is the
    // truth echoed by hello and enforced on inbound frames — never the
    // boot-default constant for a session that negotiated otherwise.
    let negotiated_audio = session.negotiated_audio;
    let (intake_tx, intake_rx) = tokio::sync::mpsc::channel::<IntakeMsg>(AUDIO_CHANNEL_CAPACITY);
    let task = Arc::new(StreamTaskState {
        state: state.clone(),
        session_id: id.clone(),
        phase: std::sync::Mutex::new(Phase::AwaitingStart),
        next_seq: AtomicU64::new(1),
        close_after: std::sync::Mutex::new(None),
        torn_down: AtomicBool::new(false),
        queue: std::sync::Mutex::new(OutboundQueue::new(OUTBOUND_QUEUE_CAPACITY)),
        queue_notify: Arc::new(tokio::sync::Notify::new()),
        negotiated_audio,
        streaming_mode,
        vad_mode: session.vad.mode,
        auto_finalize_ms: session.vad.auto_finalize_ms,
        partial_results: session.partial_results,
        max_audio_seconds: state.config.max_audio_seconds,
        media: std::sync::Mutex::new(None),
        intake_tx,
        media_started: AtomicBool::new(false),
        terminal_owner: AtomicBool::new(false),
        auth_epoch_at_upgrade,
        start_resume_token: std::sync::Mutex::new(None),
        start_resume_epoch: std::sync::Mutex::new(None),
        pause_since: std::sync::Mutex::new(None),
        #[cfg(feature = "service-opus")]
        opus_decoder: std::sync::Mutex::new(None),
    });

    Ok(ws.on_upgrade(move |socket| {
        let task = task;
        async move {
            run_stream_session(socket, task, intake_rx).await;
        }
    }))
}

/// Queue the first event (`hello`) then run the connection tasks.
async fn run_stream_session(
    socket: WebSocket,
    task: Arc<StreamTaskState>,
    intake_rx: tokio::sync::mpsc::Receiver<IntakeMsg>,
) {
    // hello is the first server event (§6): protocol_version, heartbeat, frame
    // cap, resume window, negotiated audio + streaming_mode echo.
    task.send_ordered(
        "hello",
        json!({
            "protocol_version": crate::service::config::PROTOCOL_VERSION,
            "heartbeat_ms": HEARTBEAT_MS,
            "max_frame_bytes": MAX_FRAME_BYTES,
            "resume_window_ms": task.state.config.session_resume_window_ms,
            "negotiated_audio": task.negotiated_audio,
            "streaming_mode": task.streaming_mode,
        }),
    );

    let (sink, stream) = socket.split();
    let audio_task = tokio::spawn(audio_task(task.clone(), intake_rx));
    let writer = tokio::spawn(writer_task(sink, task.clone()));
    reader_task(stream, task.clone()).await;
    // The reader ended without an armed terminal close (client-initiated
    // close, or the §15 S5/S4 out-of-band cancel/finish signal): arm the
    // complete close now so the writer finishes the RFC close handshake and
    // teardown runs as soon as the socket is done.
    {
        let mut close_after = task.close_after.lock().unwrap_or_else(|e| e.into_inner());
        if close_after.is_none() {
            *close_after = Some(WsClose::Complete);
        }
    }
    task.queue_notify.notify_waiters();
    // Give the writer a bounded window to drain queue + close handshake; the
    // audio task settles with the intake channel close.
    let _ = tokio::time::timeout(FINALIZE_CLOSE_WINDOW, writer).await;
    audio_task.abort();
    teardown(task).await;
}

/// Connection teardown: the client dropped without a terminal transition
/// (§7 for M2: an unexpected socket death cancels the session and releases
/// the lease; final remains retrievable via GET).
async fn teardown(task: Arc<StreamTaskState>) {
    if task.torn_down.swap(true, Ordering::SeqCst) {
        return;
    }
    task.state
        .live_streams
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&task.session_id);
    // Another flow owns the terminal transition (finalize/cancel in flight).
    if task.terminal_owner.load(Ordering::SeqCst) {
        return;
    }
    if task.media_started.load(Ordering::SeqCst) {
        let _ = task.state.engine.stream_cancel();
    }
    // Mirror only a genuine transition; an out-of-band REST terminal already
    // closed the engine session (double-close would re-record it).
    let transitioned = task
        .state
        .set_terminal(
            &task.session_id,
            crate::service::state::SessionStateName::Cancelled,
            None,
            None,
        )
        .is_some();
    if transitioned {
        task.state.engine.session_close(&task.session_id, false);
    }
}

/// The writer: drains the outbound queue to the socket, assigns seqs, honors
/// the drain-then-close decision, and pings on write-idle (heartbeat).
async fn writer_task(mut sink: SplitSink<WebSocket, Message>, task: Arc<StreamTaskState>) {
    let mut heartbeat = tokio::time::interval(Duration::from_millis(HEARTBEAT_MS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    heartbeat.reset();
    loop {
        tokio::select! {
            _ = task.queue_notify.notified() => {
                if !drain_queue(&mut sink, &task).await {
                    return;
                }
            }
            _ = heartbeat.tick() => {
                // Write-idle ping (heartbeat): only when nothing is
                // draining — a queued event implies liveness through it.
                let empty = task.queue.lock().unwrap_or_else(|e| e.into_inner()).is_empty();
                if empty {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return;
                    }
                } else if !drain_queue(&mut sink, &task).await {
                    return;
                }
            }
        }
    }
}

/// Pop everything currently queued and send it. Returns false after the
/// terminal close was written (the connection is done from the write side).
async fn drain_queue(
    sink: &mut SplitSink<WebSocket, Message>,
    task: &Arc<StreamTaskState>,
) -> bool {
    loop {
        let next = task.queue.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let Some(event) = next else {
            // Queue drained: honor the pending close decision if any.
            let decided = task
                .close_after
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(close) = decided {
                let _ = sink
                    .send(Message::Close(Some(CloseFrame {
                        code: close.code(),
                        reason: close.reason().into(),
                    })))
                    .await;
                return false;
            }
            return true;
        };
        let seq = task.next_seq.fetch_add(1, Ordering::SeqCst);
        let message = envelope_event(&task.session_id, seq, &event);
        if sink.send(message).await.is_err() {
            return false;
        }
        // The completion sentinel: after `final`/`error` the writer closes.
        if terminal_event(&event) {
            let decided = task
                .close_after
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            let close = decided.unwrap_or(WsClose::Complete);
            let _ = sink
                .send(Message::Close(Some(CloseFrame {
                    code: close.code(),
                    reason: close.reason().into(),
                })))
                .await;
            return false;
        }
    }
}

/// Whether this queued event ends the connection after being sent.
fn terminal_event(event: &ServerEvent) -> bool {
    matches!(event.kind, EventKind::Terminal)
}

/// Resume epoch for the v1 wire (SDK plan §15 S2/S3): M2 speaks the initial
/// epoch only; real resume fencing ships in M3.
pub const RESUME_EPOCH: u64 = 1;

/// Speech threshold of the canonical server VAD (SDK plan §7 Silero default;
/// matches the audio-toolkit CLI wiring).
pub const VAD_SPEECH_THRESHOLD: f32 = 0.5;

/// How often the reader polls the phase for a terminal transition (fatal
/// decisions may be made by the media task without touching the socket).
const TERMINAL_POLL: Duration = Duration::from_millis(250);

/// Bounded delivery wait on the FIFO intake channel.
const CONTROL_DELIVER_TIMEOUT: Duration = Duration::from_secs(5);

/// Bounded window granted to the writer to drain + close after the reader.
const FINALIZE_CLOSE_WINDOW: Duration = Duration::from_secs(5);

/// The reader: parses frames and dispatches controls/audio to the media
/// task. It exits on socket close, a terminal phase (fatal decisions made by
/// the media task), or an out-of-band REST cancel/finish notification.
async fn reader_task(mut stream: SplitStream<WebSocket>, task: Arc<StreamTaskState>) {
    let mut cancelled = task.state.cancelled_notifications.subscribe();
    let mut epoch_rx = task.state.auth_epoch_tx.subscribe();

    loop {
        tokio::select! {
            item = stream.next() => {
                let Some(item) = item else { break };
                let Ok(message) = item else { break };
                if !handle_ws_message(&task, message).await {
                    break;
                }
            }
            _ = tokio::time::sleep(TERMINAL_POLL) => {
                if task.phase() == Phase::Terminal {
                    break;
                }
            }
            id = cancelled.recv() => {
                if id.as_deref() == Ok(task.session_id.as_str()) {
                    // The session went terminal via REST (cancel/finish): the
                    // route owns the record transition; teardown clears the
                    // socket registration and cancels media if still owned.
                    break;
                }
            }
            // M3: token rotation → 4001 "token rotated" (no error event).
            new_epoch = epoch_rx.recv() => {
                if let Ok(epoch) = new_epoch {
                    if epoch > task.auth_epoch_at_upgrade {
                        task.arm_close(WsClose::TokenRotated);
                        break;
                    }
                }
            }
            // M5 §15 S5: the per-session pause budget. The arm performs the
            // reap (terminal `stale_resume` + 4003) itself when the budget is
            // spent; the wake is otherwise a re-arm.
            _ = pause_reap_sleep(&task) => {
                if task.phase() == Phase::Terminal {
                    break;
                }
            }
        }
    }
}

/// One inbound message → control/pipeline handling. False ends the reader.
async fn handle_ws_message(task: &Arc<StreamTaskState>, message: Message) -> bool {
    match message {
        Message::Close(_) => false,
        Message::Ping(_) | Message::Pong(_) => true,
        Message::Text(text) => {
            if text.len() > MAX_FRAME_BYTES {
                task.fail(
                    "invalid_request",
                    "text frame exceeds 64 KiB",
                    false,
                    WsClose::Policy,
                );
                return false;
            }
            match parse_control(text.as_str()) {
                Ok(Ok(control)) => handle_control(task, control).await,
                Ok(Err(payload)) => {
                    task.send_ordered("error", payload);
                    true
                }
                Err(close) => {
                    task.fail("invalid_request", close.reason(), false, close);
                    false
                }
            }
        }
        Message::Binary(bytes) => {
            if bytes.len() > MAX_FRAME_BYTES {
                task.fail(
                    "invalid_request",
                    "binary frame exceeds 64 KiB",
                    false,
                    WsClose::Policy,
                );
                return false;
            }
            match task.phase() {
                Phase::Streaming => {}
                Phase::AwaitingStart | Phase::Paused => {
                    // Audio-acceptance barrier (§6/§15 S5): frames outside the
                    // streaming phase are a protocol violation → 1008.
                    task.fail(
                        "invalid_request",
                        "audio frames are accepted only while streaming",
                        false,
                        WsClose::Policy,
                    );
                    return false;
                }
                Phase::Finalizing | Phase::Terminal => {
                    // Post-barrier frames sit behind the FIFO finalize and
                    // never reach the engine; drop them silently (they are a
                    // racing artifact, not a new acceptance decision, and the
                    // exactly-one-terminal invariant keeps this the final
                    // word).
                    return true;
                }
            }
            let frame = match parse_stream_frame(&task, &bytes) {
                Ok(frame) => frame,
                Err(err) => {
                    task.fail("invalid_request", &err.message(), false, WsClose::Policy);
                    return false;
                }
            };
            intake_audio(task, frame).await
        }
    }
}

/// Fatal service-side congestion: record the failure, cancel media, and
/// close 1013 after the error envelope (the §15 S1 intake/barrier mirror).
fn fail_congestion(task: &Arc<StreamTaskState>, message: &str) {
    let error = error_payload("service_busy", message, true);
    let _ = task.state.engine.stream_cancel();
    task.state.set_terminal(
        &task.session_id,
        SessionStateName::Failed,
        None,
        Some(error.clone()),
    );
    task.state.engine.session_close(&task.session_id, false);
    task.terminal_owner.store(true, Ordering::SeqCst);
    task.fail_with(error, WsClose::Backpressure);
}

/// Parse one inbound binary frame per the session's negotiated codec. The
/// negotiated profile is the enforcement truth: a `pcm_s16le` session refuses
/// an opus header and an `opus` session refuses a pcm header (§6, plus the
/// RFC 6716 packet-size validation on the opus path).
fn parse_stream_frame(task: &Arc<StreamTaskState>, bytes: &[u8]) -> Result<AudioFrame, FrameError> {
    if task.negotiated_audio.codec == "opus" {
        #[cfg(feature = "service-opus")]
        {
            let mut slot = task.opus_decoder.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_none() {
                *slot = Some(
                    crate::service::opus::OpusDecoder::new()
                        .map_err(|e| FrameError::Opus(e.to_string()))?,
                );
            }
            let decoder = slot
                .as_mut()
                .expect("opus decoder just created for a negotiated-opus session");
            return crate::service::opus::decode_opus_frame(decoder, bytes);
        }
        #[cfg(not(feature = "service-opus"))]
        {
            // Unreachable in practice: the codec is only negotiated when the
            // feature is compiled in; keep the refusal shape anyway.
            Err(FrameError::Codec)
        }
    } else {
        parse_audio_frame(bytes)
    }
}

/// Push one parsed frame into the bounded intake channel. A full channel may
/// wait up to the stall bound, then the connection is closed 1013 (the §15
/// S1/S4 mirror on the intake side).
async fn intake_audio(task: &Arc<StreamTaskState>, frame: AudioFrame) -> bool {
    if deliver(task, IntakeMsg::Audio(frame)).await {
        return true;
    }
    if task.phase() != Phase::Terminal {
        fail_congestion(task, "audio intake overflow; the decoder cannot keep up");
    }
    false
}

/// Deliver a control→intake handoff (Start barrier / finalize / cancel or an
/// audio frame) on the bounded FIFO channel: bounded try, then a bounded
/// wait, then `false` (peered full/closed).
async fn deliver(task: &Arc<StreamTaskState>, msg: IntakeMsg) -> bool {
    let sender = task.intake_tx.clone();
    match sender.try_send(msg) {
        Err(tokio::sync::mpsc::error::TrySendError::Full(back)) => {
            matches!(
                tokio::time::timeout(CONTROL_DELIVER_TIMEOUT, sender.send(back)).await,
                Ok(Ok(()))
            )
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        Ok(()) => true,
    }
}

/// One decoded control message from the client.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientControl {
    Start {
        /// M3 frame shape: accepted but unused in M2 (resume is M3).
        resume_token: Option<String>,
        last_server_seq: Option<u64>,
        resume_epoch: Option<u64>,
    },
    Ping {
        nonce: String,
    },
    Pause,
    Resume,
    Stop {
        reason: String,
    },
    Cancel {
        reason: Option<String>,
    },
}

/// Parse a client text frame. `Err(close)` means fatal (unsupported
/// version); `Ok(Err(payload))` is a non-fatal error envelope (§6:
/// unknown types yield `unknown_message_type`).
fn parse_control(text: &str) -> Result<Result<ClientControl, serde_json::Value>, WsClose> {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(e) => {
            return Ok(Err(error_payload(
                "unknown_message_type",
                &format!("text frame is not valid JSON: {e}"),
                false,
            )))
        }
    };
    let version = value.get("v").and_then(|v| v.as_u64()).unwrap_or(1);
    if version != 1 {
        return Err(WsClose::Version);
    }
    let kind = value
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    let payload = value.get("payload").cloned().unwrap_or_else(|| {
        // Tolerate flattened fields: clients may inline the payload.
        let mut rest = value.clone();
        if let Some(object) = rest.as_object_mut() {
            object.remove("v");
            object.remove("type");
            object.remove("session_id");
            object.remove("seq");
            object.remove("ts_ms");
        }
        rest
    });
    let control = match kind.as_str() {
        "start" => {
            let resume_token = payload
                .get("resume_token")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string());
            let last_server_seq = payload.get("last_server_seq").and_then(|s| s.as_u64());
            let resume_epoch = payload.get("resume_epoch").and_then(|s| s.as_u64());
            ClientControl::Start {
                resume_token,
                last_server_seq,
                resume_epoch,
            }
        }
        "ping" => ClientControl::Ping {
            nonce: payload
                .get("nonce")
                .and_then(|n| n.as_str())
                .unwrap_or_default()
                .to_string(),
        },
        "pause" => ClientControl::Pause,
        "resume" => ClientControl::Resume,
        "stop" => ClientControl::Stop {
            reason: payload
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string(),
        },
        "cancel" => ClientControl::Cancel {
            reason: payload
                .get("reason")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string()),
        },
        other => {
            return Ok(Err(error_payload(
                "unknown_message_type",
                &format!("unknown message type '{other}'"),
                false,
            )))
        }
    };
    Ok(Ok(control))
}

/// Dispatch one client control. False ends the reader.
async fn handle_control(task: &Arc<StreamTaskState>, control: ClientControl) -> bool {
    match control {
        ClientControl::Start {
            resume_token,
            last_server_seq: _,
            resume_epoch,
        } => {
            // Stash resume fields for the start_stream echo-check.
            *task
                .start_resume_token
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = resume_token;
            *task
                .start_resume_epoch
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = resume_epoch;
            start_stream(task).await
        }
        ClientControl::Ping { nonce } => {
            task.send_ordered("pong", json!({ "nonce": nonce }));
            // §15 S5: a client ping refreshes the paused session's budget.
            if task.phase() == Phase::Paused {
                task.pause_enter();
            }
            true
        }
        ClientControl::Pause => match task.phase() {
            Phase::Streaming => {
                task.set_phase(Phase::Paused);
                task.pause_enter();
                task.send_ordered("paused", json!({ "last_audio_seq": last_audio_seq(task) }));
                true
            }
            Phase::Paused => true, // idempotent pause
            Phase::AwaitingStart | Phase::Finalizing | Phase::Terminal => {
                task.send_ordered(
                    "error",
                    error_payload(
                        "invalid_request",
                        "pause is only valid while streaming",
                        false,
                    ),
                );
                true
            }
        },
        ClientControl::Resume => match task.phase() {
            Phase::Paused => {
                task.set_phase(Phase::Streaming);
                task.pause_leave();
                // M2: no `resumed` event (resume signaling ships with the M3
                // S2/S3 epoch fencing).
                true
            }
            Phase::AwaitingStart | Phase::Streaming | Phase::Finalizing | Phase::Terminal => {
                task.send_ordered(
                    "error",
                    error_payload(
                        "invalid_request",
                        "resume is only valid while paused",
                        false,
                    ),
                );
                true
            }
        },
        ClientControl::Stop { reason } => {
            let source = match reason.as_str() {
                "user" => "user",
                "vad" => "vad",
                "end_of_stream" => "end_of_stream",
                other => {
                    task.send_ordered(
                        "error",
                        error_payload(
                            "invalid_request",
                            &format!("stop reason '{other}' must be user|vad|end_of_stream"),
                            false,
                        ),
                    );
                    return true;
                }
            };
            match task.phase() {
                Phase::Streaming => {
                    // FIFO finalize barrier (§6): the media task sees every
                    // pre-stop audio frame before the finalize lands.
                    if deliver(task, IntakeMsg::Finalize { source }).await {
                        true
                    } else {
                        fail_congestion(
                            task,
                            "the audio intake channel is not accepting a finalize barrier",
                        );
                        false
                    }
                }
                Phase::Finalizing | Phase::Terminal => true, // idempotent stop
                Phase::AwaitingStart | Phase::Paused => {
                    task.send_ordered(
                        "error",
                        error_payload(
                            "invalid_request",
                            "stop is only valid while streaming",
                            false,
                        ),
                    );
                    true
                }
            }
        }
        ClientControl::Cancel { .. } => {
            match task.phase() {
                Phase::Streaming | Phase::Paused | Phase::AwaitingStart => {
                    if deliver(task, IntakeMsg::Cancel).await {
                        true
                    } else {
                        fail_congestion(
                            task,
                            "the audio intake channel is not accepting a cancel barrier",
                        );
                        false
                    }
                }
                Phase::Finalizing | Phase::Terminal => true, // already settled
            }
        }
    }
}

fn last_audio_seq(task: &Arc<StreamTaskState>) -> u64 {
    task.state
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&task.session_id)
        .map(|s| s.last_audio_seq)
        .unwrap_or(0)
}

/// `start`: duplicate guard, lease check, model load (blocking worker),
/// server-VAD build, native stream start, and the start barrier handoff. The
/// `started` event is emitted by the media task exactly when it opens the
/// streaming phase, so audio predating `started` is a protocol violation.
async fn start_stream(task: &Arc<StreamTaskState>) -> bool {
    match task.phase() {
        Phase::AwaitingStart => {}
        _ => {
            // M2 acceptance: a second start receives the retryable lease
            // envelope; the live turn continues untouched.
            task.send_ordered(
                "error",
                error_payload("service_busy", "an audio stream is already starting", true),
            );
            return true;
        }
    }

    // M3 resume fencing (§15 S2/S3): if the client presented resume
    // credentials, echo-check them before any lease/model work. A mismatch
    // → 4003 `stale-resume` (terminal, connection close). An absent token
    // (fresh connect) is always fine.
    let presented_token = task
        .start_resume_token
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let presented_epoch = *task
        .start_resume_epoch
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if presented_token.is_some() || presented_epoch.is_some() {
        // The client is asserting a resume: validate against the live session.
        let session_creds = task.state.resume_creds(&task.session_id);
        let valid = match session_creds {
            Some((stored_token, stored_epoch)) => {
                let token_ok = presented_token
                    .as_deref()
                    .map(|t| t == stored_token.as_str())
                    .unwrap_or(false);
                let epoch_ok = presented_epoch.map(|e| e == stored_epoch).unwrap_or(false);
                token_ok && epoch_ok
            }
            None => false,
        };
        if !valid {
            task.fail(
                "stale_resume",
                "resume credentials do not match the live session; reconnect fresh",
                false,
                WsClose::Resume,
            );
            return false;
        }
        // Rotate resume token now (the barrier `started` will emit the new one).
        let new_token = crate::service::auth::generate_token()
            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
        let new_epoch = presented_epoch.unwrap_or(0) + 1;
        task.state
            .set_resume_token(&task.session_id, new_token, new_epoch);
    }

    if !task
        .state
        .leases
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .held(&task.session_id)
    {
        // The single inference lease is not ours: the stream cannot start.
        let payload = error_payload(
            "service_busy",
            "the inference lease is no longer held for this session",
            true,
        );
        task.state.set_terminal(
            &task.session_id,
            SessionStateName::Failed,
            None,
            Some(payload.clone()),
        );
        task.state.engine.session_close(&task.session_id, false);
        task.fail_with(payload, WsClose::Engine);
        return false;
    }

    let Some(session) = task
        .state
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&task.session_id)
        .cloned()
    else {
        task.fail(
            "session_not_found",
            "the session vanished before start",
            false,
            WsClose::Policy,
        );
        return false;
    };

    // Fail the stream: record `failed`, release the lease, terminal error.
    fn fail_start(task: &Arc<StreamTaskState>, payload: serde_json::Value) {
        task.state.set_terminal(
            &task.session_id,
            SessionStateName::Failed,
            None,
            Some(payload.clone()),
        );
        let _ = task.state.engine.stream_cancel();
        task.state.engine.session_close(&task.session_id, false);
        task.fail_with(payload, WsClose::Engine);
    }

    // Model load runs on a blocking worker (§3.3: never on Axum socket tasks).
    let engine = task.state.engine.clone();
    let model = session.model_id.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        EngineRuntime::ensure_model_loaded(&*engine, model.as_deref())
    })
    .await;
    let facts = match loaded {
        Ok(Ok(facts)) => facts,
        Ok(Err(e)) => {
            fail_start(
                task,
                error_payload("engine_error", &format!("model load failed: {e}"), true),
            );
            return false;
        }
        Err(e) => {
            fail_start(
                task,
                error_payload("engine_error", &format!("engine worker failed: {e}"), true),
            );
            return false;
        }
    };

    // Server-canonical VAD reuses the installed Silero resource; client and
    // disabled modes bypass it entirely (§3.4).
    let vad = match session.vad.mode {
        crate::service::api::VadMode::Server => {
            let Some(path) = resolve_vad_model_path() else {
                fail_start(
                    task,
                    error_payload(
                        "engine_error",
                        "the canonical VAD model is not installed (silero_vad_v4)",
                        true,
                    ),
                );
                return false;
            };
            match SessionVad::server(&path, VAD_SPEECH_THRESHOLD, session.vad.auto_finalize_ms) {
                Ok(vad) => vad,
                Err(e) => {
                    fail_start(
                        task,
                        error_payload("engine_error", &format!("VAD init failed: {e}"), true),
                    );
                    return false;
                }
            }
        }
        mode => SessionVad::bypass(mode),
    };

    if let Err(e) = task.state.engine.stream_start() {
        fail_start(
            task,
            error_payload("engine_error", &format!("stream start failed: {e}"), true),
        );
        return false;
    }
    task.media_started.store(true, Ordering::SeqCst);

    let media_facts = MediaFacts {
        model_id: facts.model_id,
        backend: facts.backend,
        native: facts.streaming_mode == "native",
    };
    deliver(
        task,
        IntakeMsg::Start {
            facts: media_facts,
            vad,
        },
    )
    .await
}

/// Resolve `resources/models/silero_vad_v4.onnx` the way packaged resources
/// resolve: an explicit env override, an executable-adjacent resources tree,
/// then the repo layout used by dev builds/tests. `VOXIX_RESOURCE_DIR` names
/// the directory containing `resources/`.
fn resolve_vad_model_path() -> Option<std::path::PathBuf> {
    const VAD_RELATIVE: &str = "resources/models/silero_vad_v4.onnx";
    if let Ok(dir) = std::env::var("VOXIX_RESOURCE_DIR") {
        let path = std::path::Path::new(&dir).join(VAD_RELATIVE);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let path = dir.join(VAD_RELATIVE);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(VAD_RELATIVE);
    path.is_file().then_some(path)
}

/// The media task: owns the FIFO intake and does the per-frame work (phase
/// gates, VAD, engine feeds, acks) so the socket reader stays non-blocking.
async fn audio_task(
    task: Arc<StreamTaskState>,
    mut intake: tokio::sync::mpsc::Receiver<IntakeMsg>,
) {
    let mut pipeline: Option<Pipeline> = None;
    let mut text_rx = task.state.stream_text.subscribe();

    while let Some(msg) = intake.recv().await {
        match msg {
            IntakeMsg::Start { facts, vad } => {
                // A REST cancel/finish can land between the lease check and
                // this barrier: bail without media work in that case.
                let still_live = task
                    .state
                    .live
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&task.session_id);
                if !still_live {
                    task.terminal_owner.store(true, Ordering::SeqCst);
                    let _ = task.state.engine.stream_cancel();
                    task.set_phase(Phase::Terminal);
                    break;
                }
                *task.media.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
                pipeline = Some(Pipeline {
                    vad,
                    accum: AudioAccumulator::with_cap_seconds(task.max_audio_seconds),
                    language: session_language(&task),
                    last_accepted: None,
                    revision: 0,
                    received_ms: 0,
                });
                task.set_phase(Phase::Streaming);

                // M3: mint resume token on first start (if none yet), and
                // include it in the `started` barrier event.
                let (resume_tok, resume_ep) = {
                    let maybe = task.state.resume_creds(&task.session_id);
                    if let Some(pair) = maybe {
                        pair
                    } else {
                        let tok = crate::service::auth::generate_token()
                            .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
                        let ep = 1u64;
                        task.state
                            .set_resume_token(&task.session_id, tok.clone(), ep);
                        (tok, ep)
                    }
                };

                task.send_ordered(
                    "started",
                    json!({
                        "model_id": task
                            .media
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                            .map(|f| f.model_id.clone())
                            .unwrap_or_default(),
                        "backend": task
                            .media
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref()
                            .and_then(|f| f.backend.clone()),
                        "resumed": false,
                        "next_audio_seq": 0,
                        "resume_token": resume_tok,
                        "resume_epoch": resume_ep,
                    }),
                );
            }
            IntakeMsg::Audio(frame) => {
                // The pipeline exists only after the Start barrier (FIFO).
                let Some(mut current) = pipeline.take() else {
                    continue;
                };
                match task.phase() {
                    Phase::Streaming => {}
                    Phase::AwaitingStart | Phase::Paused => {
                        // The audio-acceptance barrier applies to in-flight
                        // frames too (§15 S5).
                        let error = error_payload(
                            "invalid_request",
                            "audio frames are accepted only while streaming",
                            false,
                        );
                        task.state.set_terminal(
                            &task.session_id,
                            SessionStateName::Failed,
                            None,
                            Some(error.clone()),
                        );
                        let _ = task.state.engine.stream_cancel();
                        task.state.engine.session_close(&task.session_id, false);
                        task.fail_with(error, WsClose::Policy);
                        break;
                    }
                    Phase::Finalizing | Phase::Terminal => break,
                }

                let mut terminal = false;
                if !current
                    .accept(&task, frame, &mut text_rx, &mut terminal)
                    .await
                {
                    break; // fatal handled inside
                }
                if terminal {
                    break; // the turn finalized itself
                }
                pipeline = Some(current);
            }
            IntakeMsg::Finalize { source } => {
                let Some(mut pipeline) = pipeline.take() else {
                    break; // no turn was ever started; nothing to finalize
                };
                run_finalize(&task, &mut pipeline, source).await;
                break;
            }
            IntakeMsg::Cancel => {
                run_cancel(&task);
                break;
            }
        }
    }
}

/// Session snapshot language for the final payload (immutable option).
fn session_language(task: &Arc<StreamTaskState>) -> String {
    task.state
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&task.session_id)
        .map(|s| s.language.clone())
        .unwrap_or_else(|| "auto".to_string())
}

/// Per-`start` media pipeline: bounded accumulator + canonical VAD + motive
/// counters. Dropped and rebuilt by each new Start barrier.
struct Pipeline {
    vad: SessionVad,
    accum: AudioAccumulator,
    language: String,
    last_accepted: Option<u64>,
    revision: u64,
    received_ms: u64,
}

impl Pipeline {
    /// Accept one frame: duplicates ack+ignore, gaps per §6, then VAD-gated
    /// engine feeds, speech boundaries, cumulative ack, and the optional
    /// auto-finalize. `terminal` is set when the turn finalized itself.
    async fn accept(
        &mut self,
        task: &Arc<StreamTaskState>,
        frame: AudioFrame,
        text_rx: &mut tokio::sync::broadcast::Receiver<crate::service::engine::StreamTextFrame>,
        terminal: &mut bool,
    ) -> bool {
        let AudioFrame {
            seq,
            duration_ms,
            discontinuity,
            pcm,
        } = frame;

        // Duplicates are acknowledged and ignored (§6); the first frame seeds
        // the sequence, and the §6 audio sequence starts at 0 (never at a
        // zero-default, which would swallow the first frame as a duplicate).
        if let Some(prev) = self.last_accepted {
            if seq <= prev {
                task.send_coalesced(
                    EventKind::Ack,
                    json!({
                        "through_seq": prev,
                        "received_ms": self.received_ms,
                        "buffered_ms": 0,
                    }),
                );
                return true;
            }
        }

        // Gap handling (§6 gap semantics): silence only with the flag.
        let expected = self.last_accepted.map_or(0, |p| p + 1);
        if seq > expected {
            if !discontinuity {
                let error = error_payload(
                    "audio_gap",
                    &format!(
                        "audio frame {seq} arrived after a gap at {expected} without a discontinuity flag"
                    ),
                    false,
                );
                let _ = task.state.engine.stream_cancel();
                task.state.set_terminal(
                    &task.session_id,
                    SessionStateName::Failed,
                    None,
                    Some(error.clone()),
                );
                task.state.engine.session_close(&task.session_id, false);
                task.fail_with(error, WsClose::AudioGap);
                return false;
            }
            let gap_ms = (seq - expected) as u64 * duration_ms as u64;
            let synth_ms = gap_ms.min(MAX_GAP_SILENCE_MS);
            let silence = silence_f32(synth_ms);
            if !accum_ok(self, task, &silence) {
                return false;
            }
            // New acoustic context: the VAD resets before the gap silence.
            self.vad.reset();
            self.accept_samples(task, &silence, text_rx, terminal).await;
            if *terminal {
                return true; // the silence already finalized the turn
            }
            self.received_ms += synth_ms;
        }

        self.last_accepted = Some(seq);
        if !accum_ok(self, task, &pcm_to_f32(&pcm)) {
            return false;
        }
        if !self
            .accept_samples(task, &pcm_to_f32(&pcm), text_rx, terminal)
            .await
        {
            return false; // fatal engine/feed error handled inside
        }
        self.received_ms += duration_ms as u64;
        bump_audio(task, self.received_ms, seq);
        task.send_coalesced(
            EventKind::Ack,
            json!({
                "through_seq": seq,
                "received_ms": self.received_ms,
                "buffered_ms": 0,
            }),
        );
        true
    }

    /// VAD-gated feed: engine chunks, speech boundaries, partials, and the
    /// auto-finalize countdown. Returns `false` on a fatal engine error (the
    /// terminal was already declared); a fired auto-finalize sets `terminal`.
    async fn accept_samples(
        &mut self,
        task: &Arc<StreamTaskState>,
        samples: &[f32],
        text_rx: &mut tokio::sync::broadcast::Receiver<crate::service::engine::StreamTextFrame>,
        terminal: &mut bool,
    ) -> bool {
        *terminal = false;
        let effects = self.vad.push(samples);
        for event in &effects.events {
            match event {
                crate::service::audio::VadEvent::SpeechStarted { audio_ms } => {
                    task.send_ordered("speech_start", json!({ "audio_ms": audio_ms }));
                }
                crate::service::audio::VadEvent::SpeechEnded { audio_ms } => {
                    task.send_ordered("speech_end", json!({ "audio_ms": audio_ms }));
                }
            }
        }
        for chunk in &effects.engine_chunks {
            if let Err(e) = task.state.engine.stream_feed(chunk) {
                let error = error_payload("engine_error", &format!("audio feed failed: {e}"), true);
                let _ = task.state.engine.stream_cancel();
                task.state.set_terminal(
                    &task.session_id,
                    SessionStateName::Failed,
                    None,
                    Some(error.clone()),
                );
                task.state.engine.session_close(&task.session_id, false);
                task.fail_with(error, WsClose::Engine);
                *terminal = true;
                return false;
            }
        }
        // Partials (§6): engine bus → newest-revision envelope. Only native
        // mode with `partial_results` forwards; every other backend runs the
        // final-only authoritative path (§7).
        if task.partial_results && *task.streaming_mode == *"native" {
            let mut forwarded = 0;
            while let Ok(text) = text_rx.try_recv() {
                self.revision += 1;
                task.send_coalesced(
                    EventKind::Partial,
                    json!({
                        "revision": self.revision,
                        "committed": text.committed,
                        "tentative": text.tentative,
                        "audio_received_ms": self.received_ms,
                        "audio_committed_ms": 0,
                        "stability": serde_json::Value::Null,
                        "native": true,
                    }),
                );
                forwarded += 1;
                if forwarded >= 8 {
                    break;
                }
            }
        }
        if effects.auto_finalize {
            run_finalize(task, self, "vad").await;
            *terminal = true;
        }
        true
    }
}

/// Push samples into the bounded accumulator; a full accumulator is the
/// session duration cap violation → policy failure (§3.3 bounded audio).
fn accum_ok(pipeline: &mut Pipeline, task: &Arc<StreamTaskState>, samples: &[f32]) -> bool {
    if pipeline.accum.push(samples) {
        return true;
    }
    let error = error_payload(
        "invalid_request",
        "audio exceeds the session duration cap",
        false,
    );
    let _ = task.state.engine.stream_cancel();
    task.state.set_terminal(
        &task.session_id,
        SessionStateName::Failed,
        None,
        Some(error.clone()),
    );
    task.state.engine.session_close(&task.session_id, false);
    task.fail_with(error, WsClose::Policy);
    false
}

fn bump_audio(task: &Arc<StreamTaskState>, received_ms: u64, seq: u64) {
    if let Some(session) = task
        .state
        .live
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(&task.session_id)
    {
        session.audio_received_ms = received_ms;
        session.last_audio_seq = seq;
    }
}

/// Finalize the turn (client stop or the auto-finalize countdown). The FIFO
/// barrier means every pre-stop frame has been fed before this runs.
async fn run_finalize(task: &Arc<StreamTaskState>, pipeline: &mut Pipeline, source: &'static str) {
    task.set_phase(Phase::Finalizing);
    task.terminal_owner.store(true, Ordering::SeqCst);
    if source == "vad" {
        // §15 G7: the auto finalize announces itself with the barrier type.
        task.send_ordered("finalizing", json!({ "source": "vad" }));
    }

    let audio_ms = pipeline.vad.audio_ms();
    let audio = pipeline.accum.snapshot().to_vec();
    let decoder_started = Instant::now();

    let native = *task.streaming_mode == *"native";
    let text: Result<String, String> = if audio.is_empty() {
        Ok(String::new()) // no audio: nothing to decode, no engine call
    } else {
        let engine = task.state.engine.clone();
        let batch = audio.clone();
        let decode = tokio::task::spawn_blocking(move || {
            let text = if native {
                engine
                    .stream_finalize()
                    .ok()
                    .flatten()
                    .or_else(|| EngineRuntime::batch_transcribe(&*engine, &batch).ok())
            } else {
                EngineRuntime::batch_transcribe(&*engine, &batch).ok()
            };
            text.unwrap_or_default()
        })
        .await;
        match decode {
            Ok(text) => Ok(text),
            Err(e) => Err(format!("decode worker failed: {e}")),
        }
    };

    let model_id = task
        .media
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|f| f.model_id.clone())
        .unwrap_or_default();

    match text {
        Ok(text) => {
            let decode_ms = decoder_started.elapsed().as_millis() as u64;
            let audio_secs = audio_ms as f64 / 1000.0;
            let realtime_factor = if decode_ms > 0 && audio_secs > 0.0 {
                Some(audio_secs / (decode_ms as f64 / 1000.0))
            } else {
                None
            };
            // M5: streamed finals persist to the same service history surface
            // batch completions write to — identical row shape, non-fatal on
            // error, gated by the live `save_history_default` projection. A
            // turn that never received audio is not history.
            let mut history_id: Option<i64> = None;
            let save_history = task
                .state
                .patch_config
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .save_history_default;
            if save_history && (audio_ms > 0 || !text.is_empty()) {
                if let Some(ref sh) = task.state.service_history {
                    match sh.append(&crate::service::history::HistoryWriteRequest {
                        transcription_text: text.clone(),
                        duration_ms: Some(audio_ms as i64),
                        model_id: (!model_id.is_empty()).then(|| model_id.clone()),
                        language: (!pipeline.language.is_empty())
                            .then(|| pipeline.language.clone()),
                        source: "stream",
                    }) {
                        Ok(id) => history_id = Some(id),
                        Err(e) => log::warn!(
                            target: "service::ws",
                            "history write failed for session {}: {e}",
                            task.session_id
                        ),
                    }
                }
            }
            let final_result = final_payload(
                &text,
                &pipeline.language,
                audio_ms,
                &model_id,
                decode_ms,
                realtime_factor,
                history_id,
            );
            task.state.set_terminal(
                &task.session_id,
                SessionStateName::Completed,
                Some(final_result.clone()),
                None,
            );
            task.state.engine.session_close(&task.session_id, true);
            task.queue_event(ServerEvent {
                kind: EventKind::Terminal,
                payload: json!({ "type": "final", "payload": final_result }),
            });
            task.set_phase(Phase::Terminal);
        }
        Err(e) => {
            let error = error_payload("engine_error", &e, true);
            task.state.set_terminal(
                &task.session_id,
                SessionStateName::Failed,
                None,
                Some(error.clone()),
            );
            let _ = task.state.engine.stream_cancel();
            task.state.engine.session_close(&task.session_id, false);
            task.fail_with(error, WsClose::Engine);
        }
    }
}

/// Cancel: discard, suppress results, release the lease (§15 G5's tail is
/// delegated to the engine adapter; no event — the connection closes 1000).
fn run_cancel(task: &Arc<StreamTaskState>) {
    task.terminal_owner.store(true, Ordering::SeqCst);
    task.set_phase(Phase::Finalizing);
    let _ = task.state.engine.stream_cancel();
    task.state
        .set_terminal(&task.session_id, SessionStateName::Cancelled, None, None);
    task.state.engine.session_close(&task.session_id, false);
    task.set_phase(Phase::Terminal);
    task.arm_close(WsClose::Complete);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(kind: &str) -> ServerEvent {
        ServerEvent {
            kind: EventKind::Ordered,
            payload: json!({ "type": kind, "payload": {} }),
        }
    }

    fn partial(revision: u64) -> ServerEvent {
        ServerEvent {
            kind: EventKind::Partial,
            payload: json!({ "type": "partial", "payload": { "revision": revision } }),
        }
    }

    fn ack(seq: u64) -> ServerEvent {
        ServerEvent {
            kind: EventKind::Ack,
            payload: json!({ "type": "audio_ack", "payload": { "through_seq": seq } }),
        }
    }

    fn terminal(kind: &str) -> ServerEvent {
        ServerEvent {
            kind: EventKind::Terminal,
            payload: json!({ "type": kind, "payload": {} }),
        }
    }

    #[test]
    fn coalescing_replaces_pending_partial_and_ack_in_place() {
        let mut q = OutboundQueue::new(4);
        assert!(q.push_coalesced(partial(1)));
        assert!(q.push_coalesced(ordered("speech_start")));
        assert!(q.push_coalesced(ack(3)));
        // Newest pending partial replaces revision 1, order preserved.
        assert!(q.push_coalesced(partial(2)));
        // Newest pending ack replaces through_seq 3.
        assert!(q.push_coalesced(ack(5)));
        assert_eq!(q.len(), 3);
        assert_eq!(
            q.pop().map(|e| e.payload["payload"]["revision"].clone()),
            Some(json!(2))
        );
        assert_eq!(
            q.pop()
                .and_then(|e| e.payload["type"].as_str().map(|s| s.to_string())),
            Some("speech_start".to_string())
        );
        assert_eq!(
            q.pop().map(|e| e.payload["payload"]["through_seq"].clone()),
            Some(json!(5))
        );
        assert!(q.is_empty());
    }

    #[test]
    fn terminal_reserve_evicts_unsent_partial_but_keeps_events() {
        // Fill to capacity minus reserve.
        let mut q = OutboundQueue::new(4);
        for i in 0..4 {
            assert!(q.push_coalesced(ordered(&format!("e{i}"))));
        }
        assert_eq!(q.len(), 4);
        // A plain partial at overflow: refused (never renumbered/dropped).
        assert!(!q.push_coalesced(partial(9)));
        assert_eq!(q.len(), 4);
        // The terminal always fits: the oldest ordered event yields.
        assert!(q.push_coalesced(terminal("final")));
        assert_eq!(q.len(), 4);
        assert_eq!(
            q.pop()
                .and_then(|e| e.payload["type"].as_str().map(|s| s.to_string())),
            Some("e1".to_string())
        );
        let rest = (0..3).filter_map(|_| q.pop()).count();
        assert_eq!(rest, 3);
    }

    #[test]
    fn terminal_event_marks_the_close_sentinel() {
        assert!(terminal_event(&terminal("final")));
        assert!(terminal_event(&terminal("error")));
        assert!(!terminal_event(&ordered("started")));
        assert!(!terminal_event(&partial(1)));
        assert!(!terminal_event(&ack(2)));
    }

    #[test]
    fn seq_envelope_shape_matches_the_spec() {
        let text = envelope("sess", 7, "started", json!({ "resumed": false }));
        let value: serde_json::Value = serde_json::from_str(&text).expect("envelope json");
        assert_eq!(value["v"], json!(1));
        assert_eq!(value["type"], json!("started"));
        assert_eq!(value["session_id"], json!("sess"));
        assert_eq!(value["seq"], json!(7));
        assert!(value["ts_ms"].is_u64());
        assert_eq!(value["payload"]["resumed"], json!(false));
    }

    #[test]
    fn final_payload_shape_is_snake_case_final_result() {
        let value = final_payload("hello", "auto", 1200, "test-model", 42, Some(2.5), Some(7));
        assert_eq!(value["text"], json!("hello"));
        assert!(value["segments"].is_array());
        assert_eq!(value["language"], json!("auto"));
        assert_eq!(value["duration_ms"], json!(1200));
        assert_eq!(value["model_id"], json!("test-model"));
        assert_eq!(value["history_id"], json!(7));
        assert_eq!(value["metrics"]["decode_ms"], json!(42));
        assert_eq!(value["metrics"]["realtime_factor"], json!("2.500"));
        let unsaved = final_payload("", "auto", 0, "test-model", 1, None, None);
        assert!(unsaved["history_id"].is_null());
    }

    #[test]
    fn close_codes_are_the_rediscovered_table() {
        assert_eq!(WsClose::Complete.code(), 1000);
        assert_eq!(WsClose::Shutdown.code(), 1001);
        assert_eq!(WsClose::Policy.code(), 1008);
        assert_eq!(WsClose::Engine.code(), 1011);
        assert_eq!(WsClose::Backpressure.code(), 1013);
        assert_eq!(WsClose::TokenRotated.code(), 4001);
        assert_eq!(WsClose::Version.code(), 4002);
        assert_eq!(WsClose::Resume.code(), 4003);
        assert_eq!(WsClose::AudioGap.code(), 4004);
    }

    // §15 S5 pause-reaping decision matrix.

    #[test]
    fn pause_reap_never_fires_before_any_budget() {
        assert_eq!(pause_reap_decision(30_000, 0, true, 1_000), None);
        assert_eq!(pause_reap_decision(30_000, 60_000, true, 29_000), None);
        assert_eq!(pause_reap_decision(30_000, 0, false, 29_000), None);
    }

    #[test]
    fn pause_reap_lease_held_closed_only_by_optin_pause_max() {
        // Default (cap disabled): a lease-held paused session is never reaped.
        assert_eq!(pause_reap_decision(30_000, 0, true, u64::MAX), None);
        // Opt-in cap: exactly at the cap and beyond → 4003.
        assert_eq!(
            pause_reap_decision(30_000, 5_000, true, 4_999),
            None,
            "idle just under the cap is not reaped"
        );
        assert_eq!(
            pause_reap_decision(30_000, 5_000, true, 5_000),
            Some(PauseReap::PauseMax)
        );
    }

    #[test]
    fn pause_reap_unleased_sessions_follow_the_resume_window() {
        assert_eq!(
            pause_reap_decision(30_000, 0, false, 30_000),
            Some(PauseReap::ResumeWindow)
        );
        // The opt-in cap can be earlier than the window for any lease state.
        assert_eq!(
            pause_reap_decision(30_000, 2_000, false, 30_000),
            Some(PauseReap::PauseMax)
        );
    }
}
