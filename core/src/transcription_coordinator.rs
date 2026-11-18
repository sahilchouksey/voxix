use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use log::{debug, error, warn};
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const DEBOUNCE: Duration = Duration::from_millis(30);
const RELEASE_GRACE: Duration = Duration::from_millis(50);
/// A deliberate second press lands ~150-300ms after the first release, so
/// 350ms admits real double taps while a genuinely separate action never
/// locks.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(350);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttAction {
    Passthrough,
    DeferRelease,
    CancelRelease,
}

struct PendingRelease {
    binding_id: String,
    hotkey_string: String,
    deadline: Instant,
}

/// Per-tap timing for one binding (press → release duration).
#[derive(Default)]
struct Tap {
    press_start: Option<Instant>,
}

/// A unified "wait for the next event or deadline" slot.
enum PendingAction {
    /// Existing: PTT release deferred 50ms to absorb X11 auto-repeat.
    DeferRelease(PendingRelease),
    /// New: an eligible tap was released; a second press within the window
    /// locks the recording.
    TapWindow {
        binding_id: String,
        hotkey_string: String,
        deadline: Instant,
    },
}

/// Double-tap lock settings, read fresh per event so a settings toggle takes
/// effect immediately without re-registering shortcuts.
struct TapSettings {
    lock: bool,
    only: bool,
    min_t: Duration,
}

fn tap_settings(app: &AppHandle) -> TapSettings {
    let s = crate::settings::get_settings(app);
    TapSettings {
        lock: s.double_tap_lock_enabled,
        only: s.double_tap_lock_enabled && s.use_double_tap_only,
        min_t: Duration::from_secs_f64(s.minimum_key_time.max(0.0)),
    }
}

/// A tap is eligible when its press lasted at least `min_t`; a missing press
/// start (lost event) counts as eligible.
fn tap_is_eligible(
    taps: &HashMap<String, Tap>,
    binding_id: &str,
    now: Instant,
    min_t: Duration,
) -> bool {
    taps.get(binding_id)
        .and_then(|tap| tap.press_start)
        .is_none_or(|start| now.duration_since(start) >= min_t)
}

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input {
        binding_id: String,
        hotkey_string: String,
        is_pressed: bool,
        push_to_talk: bool,
    },
    Cancel {
        recording_was_active: bool,
    },
    ProcessingFinished,
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
enum Stage {
    Idle,
    Recording(String), // binding_id
    Processing,
}

fn classify_ptt_event(
    pending_release_binding: Option<&str>,
    is_pressed: bool,
    push_to_talk: bool,
    binding_id: &str,
    recording_binding: Option<&str>,
) -> PttAction {
    if !push_to_talk {
        return PttAction::Passthrough;
    }

    if is_pressed {
        if pending_release_binding == Some(binding_id) {
            PttAction::CancelRelease
        } else {
            PttAction::Passthrough
        }
    } else if recording_binding == Some(binding_id) && pending_release_binding.is_none() {
        PttAction::DeferRelease
    } else {
        PttAction::Passthrough
    }
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
}

pub fn is_transcribe_binding(id: &str) -> bool {
    id == "transcribe" || id == "transcribe_with_post_process"
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut stage = Stage::Idle;
                let mut last_press: Option<Instant> = None;
                let mut pending: Option<PendingAction> = None;
                let mut taps: HashMap<String, Tap> = HashMap::new();
                let mut locked: Option<String> = None; // binding whose recording is hands-free-locked

                loop {
                    let deadline = pending.as_ref().map(|action| match action {
                        PendingAction::DeferRelease(release) => release.deadline,
                        PendingAction::TapWindow { deadline, .. } => *deadline,
                    });

                    let cmd = if let Some(deadline) = deadline {
                        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            Ok(cmd) => cmd,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if let Some(action) = pending.take() {
                                    match action {
                                        PendingAction::DeferRelease(release) => {
                                            if matches!(&stage, Stage::Recording(id) if id == &release.binding_id)
                                            {
                                                let settings = tap_settings(&app);
                                                if settings.lock && !settings.only {
                                                    // A too-short release is an
                                                    // ineligible accidental tap:
                                                    // drop it silently. An
                                                    // eligible one arms the
                                                    // double-tap window and lets
                                                    // the recording keep running.
                                                    if tap_is_eligible(
                                                        &taps,
                                                        &release.binding_id,
                                                        Instant::now(),
                                                        settings.min_t,
                                                    ) {
                                                        pending = Some(PendingAction::TapWindow {
                                                            binding_id: release.binding_id,
                                                            hotkey_string: release.hotkey_string,
                                                            deadline: Instant::now()
                                                                + DOUBLE_TAP_WINDOW,
                                                        });
                                                    } else {
                                                        if let Some(tap) =
                                                            taps.get_mut(&release.binding_id)
                                                        {
                                                            tap.press_start = None;
                                                        }
                                                        discard(&app, &mut stage);
                                                    }
                                                } else {
                                                    stop(
                                                        &app,
                                                        &mut stage,
                                                        &release.binding_id,
                                                        &release.hotkey_string,
                                                    );
                                                }
                                            }
                                        }
                                        PendingAction::TapWindow {
                                            binding_id,
                                            hotkey_string,
                                            ..
                                        } => {
                                            // A single eligible tap transcribes
                                            // exactly as a plain PTT/tap would;
                                            // in double-tap-only mode the window
                                            // may have armed with nothing
                                            // recording, in which case no-op.
                                            if matches!(&stage, Stage::Recording(id) if id == &binding_id)
                                            {
                                                stop(&app, &mut stage, &binding_id, &hotkey_string);
                                            }
                                        }
                                    }
                                }
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        match rx.recv() {
                            Ok(cmd) => cmd,
                            Err(_) => break,
                        }
                    };

                    match cmd {
                        Command::Input {
                            binding_id,
                            hotkey_string,
                            is_pressed,
                            push_to_talk,
                        } => {
                            let settings = tap_settings(&app);
                            let lock = settings.lock;
                            let only = settings.only;
                            let min_t = settings.min_t;
                            let now = Instant::now();

                            // While locked the recording is hands-free:
                            // releases of the locked binding are ignored, only
                            // a single press (or cancel) ends it.
                            if !is_pressed && locked.as_deref() == Some(binding_id.as_str()) {
                                continue;
                            }

                            let pending_release_binding = match &pending {
                                Some(PendingAction::DeferRelease(release)) => {
                                    Some(release.binding_id.as_str())
                                }
                                _ => None,
                            };
                            let recording_binding = match &stage {
                                Stage::Recording(id) => Some(id.as_str()),
                                _ => None,
                            };

                            match classify_ptt_event(
                                pending_release_binding,
                                is_pressed,
                                push_to_talk,
                                &binding_id,
                                recording_binding,
                            ) {
                                PttAction::CancelRelease => {
                                    pending = None;
                                    // A qualifying press inside the grace
                                    // window is the second tap of a double tap:
                                    // lock the still-running recording.
                                    // Auto-repeat presses that cancel the release
                                    // after a short tap are ineligible (the
                                    // press lasted a few ms) and never lock.
                                    if lock
                                        && !only
                                        && tap_is_eligible(&taps, &binding_id, now, min_t)
                                    {
                                        locked = Some(binding_id);
                                    }
                                    continue;
                                }
                                PttAction::DeferRelease => {
                                    pending = Some(PendingAction::DeferRelease(PendingRelease {
                                        binding_id,
                                        hotkey_string,
                                        deadline: now + RELEASE_GRACE,
                                    }));
                                    continue;
                                }
                                PttAction::Passthrough => {}
                            }

                            // Debounce rapid-fire press events (key repeat / double-tap).
                            // Push-to-talk releases may be deferred above to absorb X11 auto-repeat.
                            if is_pressed {
                                if last_press.is_some_and(|t| now.duration_since(t) < DEBOUNCE) {
                                    debug!("Debounced press for '{binding_id}'");
                                    continue;
                                }
                                last_press = Some(now);
                                taps.entry(binding_id.clone()).or_default().press_start = Some(now);
                            }

                            if is_pressed && lock {
                                // A single press while locked stops the
                                // hands-free recording.
                                if locked.as_deref() == Some(binding_id.as_str()) {
                                    stop(&app, &mut stage, &binding_id, &hotkey_string);
                                    locked = None;
                                    continue;
                                }
                                // Second qualifying press within the tap window
                                // locks the recording on; in double-tap-only
                                // mode nothing is recording yet, so it starts.
                                if let Some(PendingAction::TapWindow {
                                    binding_id: armed, ..
                                }) = &pending
                                {
                                    if armed == &binding_id {
                                        pending = None;
                                        if matches!(stage, Stage::Idle) {
                                            start(&app, &mut stage, &binding_id, &hotkey_string);
                                        }
                                        locked = Some(binding_id);
                                        continue;
                                    }
                                }
                                // Double-tap-only: hold-to-talk and
                                // tap-to-toggle are disabled entirely; only a
                                // double tap can start a recording.
                                if only && matches!(stage, Stage::Idle) {
                                    continue;
                                }
                            }

                            // A genuine release under the lock (hold-to-talk
                            // only): an ineligible (too-short) tap is discarded
                            // silently, an eligible one arms the double-tap
                            // window and leaves the recording, if any, running.
                            // In double-tap-only mode the first tap of a double
                            // tap arms the window from idle. Toggle mode ignores
                            // releases entirely — presses do all the toggling.
                            if !is_pressed && push_to_talk && lock && locked.is_none() {
                                let recording_ours =
                                    matches!(&stage, Stage::Recording(id) if id == &binding_id);
                                if recording_ours || (only && matches!(stage, Stage::Idle)) {
                                    if !tap_is_eligible(&taps, &binding_id, now, min_t) {
                                        if let Some(tap) = taps.get_mut(&binding_id) {
                                            tap.press_start = None;
                                        }
                                        if recording_ours {
                                            discard(&app, &mut stage);
                                        }
                                        continue;
                                    }
                                    pending = Some(PendingAction::TapWindow {
                                        binding_id: binding_id.clone(),
                                        hotkey_string: hotkey_string.clone(),
                                        deadline: now + DOUBLE_TAP_WINDOW,
                                    });
                                    continue;
                                }
                            }

                            if push_to_talk {
                                if is_pressed && matches!(stage, Stage::Idle) {
                                    start(&app, &mut stage, &binding_id, &hotkey_string);
                                } else if !is_pressed
                                    && matches!(&stage, Stage::Recording(id) if id == &binding_id)
                                {
                                    stop(&app, &mut stage, &binding_id, &hotkey_string);
                                }
                            } else if is_pressed {
                                match &stage {
                                    Stage::Idle => {
                                        start(&app, &mut stage, &binding_id, &hotkey_string);
                                    }
                                    Stage::Recording(id) if id == &binding_id => {
                                        stop(&app, &mut stage, &binding_id, &hotkey_string);
                                    }
                                    _ => {
                                        debug!("Ignoring press for '{binding_id}': pipeline busy")
                                    }
                                }
                            }
                        }
                        Command::Cancel {
                            recording_was_active,
                        } => {
                            pending = None;
                            locked = None;
                            // Don't reset during processing — wait for the pipeline to finish.
                            if !matches!(stage, Stage::Processing)
                                && (recording_was_active || matches!(stage, Stage::Recording(_)))
                            {
                                stage = Stage::Idle;
                            }
                        }
                        Command::ProcessingFinished => {
                            stage = Stage::Idle;
                        }
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {e:?}");
            }
        });

        Self { tx }
    }

    /// Send a keyboard/signal input event for a transcribe binding.
    /// For signal-based toggles, use `is_pressed: true` and `push_to_talk: false`.
    pub fn send_input(
        &self,
        binding_id: &str,
        hotkey_string: &str,
        is_pressed: bool,
        push_to_talk: bool,
    ) {
        if self
            .tx
            .send(Command::Input {
                binding_id: binding_id.to_string(),
                hotkey_string: hotkey_string.to_string(),
                is_pressed,
                push_to_talk,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self
            .tx
            .send(Command::Cancel {
                recording_was_active,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_processing_finished(&self) {
        if self.tx.send(Command::ProcessingFinished).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

fn start(app: &AppHandle, stage: &mut Stage, binding_id: &str, hotkey_string: &str) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|a| a.is_recording())
    {
        *stage = Stage::Recording(binding_id.to_string());
    } else {
        debug!("Start for '{binding_id}' did not begin recording; staying idle");
    }
}

fn stop(app: &AppHandle, stage: &mut Stage, binding_id: &str, hotkey_string: &str) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.stop(app, binding_id, hotkey_string);
    *stage = Stage::Processing;
}

/// Stop a recording and drop it — no transcription pipeline run.
/// Used for ineligible (too-short) taps under the double-tap lock. No cancel
/// sound is played: a discarded accidental tap must be silent.
fn discard(app: &AppHandle, stage: &mut Stage) {
    if let Some(audio) = app.try_state::<Arc<AudioRecordingManager>>() {
        audio.cancel_recording();
    }
    crate::utils::hide_recording_overlay(app);
    crate::utils::change_tray_icon(app, crate::tray::TrayIconState::Idle);
    *stage = Stage::Idle;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_release_while_recording_defers_release() {
        assert_eq!(
            classify_ptt_event(None, false, true, "transcribe", Some("transcribe")),
            PttAction::DeferRelease
        );
    }

    #[test]
    fn push_to_talk_press_matching_pending_release_cancels_release() {
        assert_eq!(
            classify_ptt_event(
                Some("transcribe"),
                true,
                true,
                "transcribe",
                Some("transcribe")
            ),
            PttAction::CancelRelease
        );
    }

    #[test]
    fn toggle_mode_press_and_release_pass_through() {
        assert_eq!(
            classify_ptt_event(
                Some("transcribe"),
                true,
                false,
                "transcribe",
                Some("transcribe")
            ),
            PttAction::Passthrough
        );
        assert_eq!(
            classify_ptt_event(None, false, false, "transcribe", Some("transcribe")),
            PttAction::Passthrough
        );
    }

    #[test]
    fn press_for_different_binding_than_pending_release_passes_through() {
        assert_eq!(
            classify_ptt_event(
                Some("transcribe"),
                true,
                true,
                "transcribe_with_post_process",
                Some("transcribe")
            ),
            PttAction::Passthrough
        );
    }

    #[test]
    fn press_matching_pending_release_cancels_without_recording_state() {
        assert_eq!(
            classify_ptt_event(Some("transcribe"), true, true, "transcribe", None),
            PttAction::CancelRelease
        );
    }

    // ---------------------------------------------------------------------
    // Sequence-level regression coverage for issue #1539.
    //
    // Under X11 key auto-repeat, holding a push-to-talk key does not emit one
    // long press. It emits the initial press followed by a stream of
    // synthesized release/press pairs, then a single genuine release on key-up.
    // Before the fix, every synthesized release passed straight through and
    // stopped recording, so holding the key "rapidly toggled" recording on and
    // off. The fix defers each release for a short grace window and cancels it
    // when the matching auto-repeat press arrives.
    //
    // The unit tests above assert `classify_ptt_event` in isolation. The
    // simulator below threads that classifier through the same `pending_release`
    // / `stage` state transitions the coordinator loop performs (lines that
    // handle `Command::Input` and the `recv_timeout` grace expiry), so a whole
    // event burst can be exercised deterministically without a Tauri AppHandle
    // or real timers.
    // ---------------------------------------------------------------------

    const BINDING: &str = "transcribe";

    #[derive(Clone, Copy)]
    enum Ev {
        /// A key-down event (real initial press or a synthesized auto-repeat press).
        Press,
        /// A key-up event (synthesized auto-repeat release or the genuine key-up).
        Release,
        /// The `RELEASE_GRACE` window elapsed with no cancelling press arriving.
        Grace,
        /// The `DOUBLE_TAP_WINDOW` elapsed with no second press arriving.
        TapWindow,
        /// Time passes with no key event (spacers to clear the debounce
        /// between deliberate presses; no key event can arrive while the key
        /// is physically up).
        Wait,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SimStage {
        Idle,
        Recording,
        Processing,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum SimPending {
        DeferRelease,
        TapWindow,
    }

    struct SimResult {
        starts: u32,
        stops: u32,
        stage: SimStage,
        locked: bool,
        discards: u32,
    }

    /// Mirror of the coordinator loop's decision logic for a single push-to-talk
    /// binding: it calls the real `classify_ptt_event` and applies the exact same
    /// Defer / Cancel / debounce / start / stop / discard / lock transitions the
    /// `Command::Input` arm and the `recv_timeout` arms perform, so a whole event
    /// burst can be exercised deterministically without a Tauri AppHandle or real
    /// timers. With `lock: false` the transitions are byte-for-byte the legacy
    /// PTT behavior.
    fn simulate(events: &[Ev], lock: bool, only: bool, min_t_ms: u64) -> SimResult {
        simulate_mode(events, lock, only, min_t_ms, true)
    }

    fn simulate_mode(
        events: &[Ev],
        lock: bool,
        only: bool,
        min_t_ms: u64,
        push_to_talk: bool,
    ) -> SimResult {
        let only = lock && only;
        let mut stage = SimStage::Idle;
        let mut pending: Option<SimPending> = None;
        let mut locked = false;
        let mut press_start_ms: Option<u64> = None;
        let mut last_press_ms: Option<u64> = None;
        let mut clock_ms: u64 = 0;
        let mut starts = 0u32;
        let mut stops = 0u32;
        let mut discards = 0u32;
        let debounce_ms = DEBOUNCE.as_millis() as u64;

        for ev in events {
            // Auto-repeat events arrive a few ms apart, well inside DEBOUNCE.
            clock_ms += 5;
            let now = clock_ms;

            match ev {
                Ev::Grace => {
                    // The `DeferRelease` timeout arm.
                    if let Some(SimPending::DeferRelease) = pending.take() {
                        if stage == SimStage::Recording {
                            if lock && !only {
                                let eligible =
                                    press_start_ms.is_some_and(|start| now - start >= min_t_ms);
                                if eligible {
                                    pending = Some(SimPending::TapWindow);
                                } else {
                                    press_start_ms = None;
                                    stage = SimStage::Idle;
                                    discards += 1;
                                }
                            } else {
                                stage = SimStage::Processing;
                                stops += 1;
                            }
                        }
                    }
                }
                Ev::TapWindow => {
                    // The `TapWindow` timeout arm: stop iff the window armed
                    // with an active recording.
                    if let Some(SimPending::TapWindow) = pending.take() {
                        if stage == SimStage::Recording {
                            stage = SimStage::Processing;
                            stops += 1;
                        }
                    }
                }
                Ev::Wait => {
                    // The clock already advanced above; nothing else happens.
                }
                Ev::Press | Ev::Release => {
                    let is_pressed = matches!(ev, Ev::Press);

                    // While locked, releases of the binding are ignored.
                    if !is_pressed && locked {
                        continue;
                    }

                    let pending_release = matches!(pending, Some(SimPending::DeferRelease));
                    let recording_binding = if stage == SimStage::Recording {
                        Some(BINDING)
                    } else {
                        None
                    };

                    match classify_ptt_event(
                        pending_release.then_some(BINDING),
                        is_pressed,
                        push_to_talk,
                        BINDING,
                        recording_binding,
                    ) {
                        PttAction::CancelRelease => {
                            pending = None;
                            // A qualifying press inside the grace window is the
                            // second tap of a double tap: lock the recording.
                            if lock
                                && !only
                                && press_start_ms.is_none_or(|start| now - start >= min_t_ms)
                            {
                                locked = true;
                            }
                            continue;
                        }
                        PttAction::DeferRelease => {
                            pending = Some(SimPending::DeferRelease);
                            continue;
                        }
                        PttAction::Passthrough => {}
                    }

                    if is_pressed {
                        if last_press_ms.is_some_and(|t| now - t < debounce_ms) {
                            continue;
                        }
                        last_press_ms = Some(now);
                        press_start_ms = Some(now);
                    }

                    if is_pressed && lock {
                        if locked {
                            // A single press while locked stops the recording.
                            stage = SimStage::Processing;
                            stops += 1;
                            locked = false;
                            continue;
                        }
                        if matches!(pending, Some(SimPending::TapWindow)) {
                            pending = None;
                            if stage == SimStage::Idle {
                                stage = SimStage::Recording;
                                starts += 1;
                            }
                            locked = true;
                            continue;
                        }
                        if only && stage == SimStage::Idle {
                            // Double-tap-only: a lone press starts nothing.
                            continue;
                        }
                    }

                    if !is_pressed && push_to_talk && lock && !locked {
                        let recording_ours = stage == SimStage::Recording;
                        if recording_ours || (only && stage == SimStage::Idle) {
                            if press_start_ms.is_some_and(|start| now - start < min_t_ms) {
                                press_start_ms = None;
                                if recording_ours {
                                    stage = SimStage::Idle;
                                    discards += 1;
                                }
                                continue;
                            }
                            pending = Some(SimPending::TapWindow);
                            continue;
                        }
                    }

                    if push_to_talk {
                        if is_pressed && stage == SimStage::Idle {
                            stage = SimStage::Recording;
                            starts += 1;
                        } else if !is_pressed && stage == SimStage::Recording {
                            stage = SimStage::Processing;
                            stops += 1;
                        }
                    } else if is_pressed {
                        match stage {
                            SimStage::Idle => {
                                stage = SimStage::Recording;
                                starts += 1;
                            }
                            SimStage::Recording => {
                                stage = SimStage::Processing;
                                stops += 1;
                            }
                            SimStage::Processing => {}
                        }
                    }
                }
            }
        }

        SimResult {
            starts,
            stops,
            stage,
            locked,
            discards,
        }
    }

    /// Initial press plus several synthesized release/press pairs, as X11 emits
    /// while a push-to-talk key is held down.
    fn autorepeat_burst() -> Vec<Ev> {
        let mut events = vec![Ev::Press];
        for _ in 0..6 {
            events.push(Ev::Release);
            events.push(Ev::Press);
        }
        events
    }

    /// Regression for #1539: a burst of X11 auto-repeat release/press pairs must
    /// not stop recording. Before the fix the first synthesized release stopped
    /// recording immediately (stops == 1, stage left Recording), which produced
    /// the rapid on/off toggling. With the fix the releases are coalesced and
    /// recording stays continuously active for the whole burst.
    #[test]
    fn x11_autorepeat_burst_does_not_toggle_recording() {
        let result = simulate(&autorepeat_burst(), false, false, 0);
        assert_eq!(result.starts, 1, "recording should start exactly once");
        assert_eq!(
            result.stops, 0,
            "synthesized auto-repeat releases must not stop recording mid-burst"
        );
        assert_eq!(
            result.stage,
            SimStage::Recording,
            "recording must remain active across the entire auto-repeat burst"
        );
        assert!(!result.locked);
        assert_eq!(result.discards, 0);
    }

    /// Complements the burst test: once the key is genuinely released and the
    /// grace window elapses with no re-press, recording stops exactly once. This
    /// proves the debounce only coalesces synthesized releases and does not wedge
    /// the coordinator or swallow the real key-up.
    #[test]
    fn genuine_release_after_grace_stops_recording_once() {
        let mut events = autorepeat_burst();
        events.push(Ev::Release); // genuine key-up
        events.push(Ev::Grace); // grace window elapses, no cancelling press
        let result = simulate(&events, false, false, 0);
        assert_eq!(result.starts, 1, "recording should start exactly once");
        assert_eq!(
            result.stops, 1,
            "a genuine release should stop recording exactly once"
        );
        assert_eq!(result.stage, SimStage::Processing);
        assert!(!result.locked);
        assert_eq!(result.discards, 0);
    }

    // ---------------------------------------------------------------------
    // Double-tap lock regression tests. `min_t_ms: 0` makes every 5ms
    // simulator tap eligible; the short-tap test uses `min_t_ms: 20` so the
    // 5ms tap is ineligible. The 30ms debounce is cleared by spacing presses
    // with (ignored) releases, which mirror the physical key going up between
    // deliberate presses.
    // ---------------------------------------------------------------------

    /// (1) Two eligible taps with the second press inside the window: the first
    /// press starts once, the release arms the window via the grace expiry, and
    /// the second press locks — no stop yet, recording runs hands-free.
    #[test]
    fn double_tap_within_window_locks_recording() {
        let events = vec![
            Ev::Press,   // t=5 starts
            Ev::Release, // t=10 defers
            Ev::Grace,   // t=15 arms the tap window
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Press, // t=40: second press, locks
        ];
        let result = simulate(&events, true, false, 0);
        assert_eq!(result.starts, 1, "recording should start exactly once");
        assert_eq!(result.stops, 0, "the lock must not stop the recording");
        assert!(result.locked, "the second press must lock the recording");
        assert_eq!(result.stage, SimStage::Recording);
    }

    /// (2) A single press while locked stops the recording exactly once and
    /// clears the lock. The release of the locking tap arrives while locked
    /// and must be ignored.
    #[test]
    fn press_while_locked_stops_recording() {
        let events = vec![
            Ev::Press,   // t=5 starts
            Ev::Release, // t=10 defers
            Ev::Grace,   // t=15 arms the tap window
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Press,   // t=40: second press, locks
            Ev::Release, // t=45: release while locked, ignored
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Press, // t=70: the single stop-tap
        ];
        let result = simulate(&events, true, false, 0);
        assert_eq!(result.starts, 1);
        assert_eq!(result.stops, 1, "the stop-tap must stop exactly once");
        assert!(!result.locked, "the lock must be cleared by the stop-tap");
        assert_eq!(result.stage, SimStage::Processing);
    }

    /// (3) A single eligible tap with no second press: the window expires and
    /// the recording stops (transcribes) exactly once.
    #[test]
    fn single_eligible_tap_transcribes_when_window_expires() {
        let events = vec![Ev::Press, Ev::Release, Ev::Grace, Ev::TapWindow];
        let result = simulate(&events, true, false, 0);
        assert_eq!(result.starts, 1);
        assert_eq!(result.stops, 1, "window expiry must stop the single tap");
        assert!(!result.locked);
        assert_eq!(result.stage, SimStage::Processing);
    }

    /// (4) A tap shorter than `minimum_key_time` is discarded silently: no
    /// stop, no processing, no lock.
    #[test]
    fn short_tap_below_minimum_key_time_is_discarded() {
        let events = vec![Ev::Press, Ev::Release, Ev::Grace];
        let result = simulate(&events, true, false, 20);
        assert_eq!(result.discards, 1, "the short tap must be discarded");
        assert_eq!(result.stops, 0, "a discarded tap must not transcribe");
        assert!(!result.locked);
        assert_eq!(result.stage, SimStage::Idle);
    }

    /// (5a) Double-tap-only mode: a single tap starts nothing, even after the
    /// window expires.
    #[test]
    fn double_tap_only_single_tap_starts_nothing() {
        let events = vec![Ev::Press, Ev::Release, Ev::TapWindow];
        let result = simulate(&events, true, true, 0);
        assert_eq!(result.starts, 0, "a lone tap must not start recording");
        assert_eq!(result.stops, 0);
        assert!(!result.locked);
        assert_eq!(result.stage, SimStage::Idle);
    }

    /// (5b) Double-tap-only mode: a double tap starts the recording (on the
    /// second press, which also locks) and runs hands-free.
    #[test]
    fn double_tap_only_double_tap_starts_and_locks() {
        let events = vec![
            Ev::Press,   // t=5: lone press, no start
            Ev::Release, // t=10: arms the window from idle
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Press, // t=40: second press, start + lock
        ];
        let result = simulate(&events, true, true, 0);
        assert_eq!(result.starts, 1, "the double tap must start exactly once");
        assert_eq!(result.stops, 0);
        assert!(result.locked, "the double tap must lock the recording");
        assert_eq!(result.stage, SimStage::Recording);
    }

    /// (6) Lock disabled: a plain PTT tap stops on the grace expiry exactly as
    /// before the feature existed (covered by the auto-repeat tests above, this
    /// pins the single-tap shape).
    #[test]
    fn lock_disabled_keeps_legacy_ptt_behavior() {
        let events = vec![Ev::Press, Ev::Release, Ev::Grace];
        let result = simulate(&events, false, false, 20);
        assert_eq!(result.starts, 1);
        assert_eq!(result.stops, 1, "lock off must stop on the grace expiry");
        assert!(!result.locked);
        assert_eq!(
            result.discards, 0,
            "minimum_key_time must not apply when off"
        );
        assert_eq!(result.stage, SimStage::Processing);
    }

    /// Toggle mode + double-tap lock ON: a short tap (press → release < min_t)
    /// must NOT discard the recording and must NOT arm the tap window. The
    /// recording keeps running; the release is a complete no-op.
    #[test]
    fn toggle_mode_short_release_does_not_discard_or_arm() {
        let events = vec![Ev::Press, Ev::Release];
        let result = simulate_mode(&events, true, false, 20, false);
        assert_eq!(result.starts, 1, "the press must start the recording");
        assert_eq!(result.discards, 0, "toggle releases must never discard");
        assert_eq!(result.stops, 0, "toggle releases must never auto-stop");
        assert_eq!(
            result.stage,
            SimStage::Recording,
            "recording must keep running after a short tap-and-release"
        );
        assert!(!result.locked);
    }

    /// Toggle mode: an eligible long press → release followed by window
    /// expiry must not auto-stop either.
    #[test]
    fn toggle_mode_release_and_timeout_does_not_stop() {
        let events = vec![Ev::Press, Ev::Release, Ev::Wait, Ev::TapWindow];
        let result = simulate_mode(&events, true, false, 0, false);
        assert_eq!(result.stops, 0, "toggle releases never arm the tap window");
        assert_eq!(result.stage, SimStage::Recording);
    }

    /// Toggle mode: second press stops the recording exactly once.
    #[test]
    fn toggle_mode_second_press_stops() {
        let events = vec![
            Ev::Press,
            Ev::Release,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait,
            Ev::Wait, // clear the debounce
            Ev::Press,
        ];
        let result = simulate_mode(&events, true, false, 0, false);
        assert_eq!(result.starts, 1);
        assert_eq!(result.stops, 1, "the second press must stop exactly once");
        assert_eq!(result.stage, SimStage::Processing);
        assert!(!result.locked);
        assert_eq!(result.discards, 0);
    }

    /// Toggle mode with the lock disabled behaves the same on releases.
    #[test]
    fn toggle_mode_without_lock_release_is_noop() {
        let events = vec![Ev::Press, Ev::Release, Ev::TapWindow];
        let result = simulate_mode(&events, false, false, 0, false);
        assert_eq!(result.stops, 0);
        assert_eq!(result.stage, SimStage::Recording);
    }
}
