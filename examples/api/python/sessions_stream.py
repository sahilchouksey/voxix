#!/usr/bin/env python3
# sessions_stream.py — streaming transcription over the WS media plane.
#
# SDK twin: ../../sdk/05-streaming.ts — same flow with the typed SDK client.
# This file is the raw-wire equivalent for people who want to see the protocol
# by hand. It REQUIRES the third-party `websockets` package; the api/ tree is
# otherwise stdlib-only on purpose.
#
# Demonstrates the hello → start → frames → stop("user") → exactly-one `final`
# discipline ( §11):
#   1. POST /v1/sessions {"vad":{"mode":"server"},"partial_results":true} → 201
#      (carry the one-time session_token)
#   2. WS upgrade GET /v1/sessions/{id}/stream with bearer + X-Session-Token
#      → server `hello` (barrier: no start before hello)
#   3. send {"type":"start","payload":{}} → server `started` (barrier: NO AUDIO BEFORE THIS)
#   4. push ~20 ms binary frames: "VX" + 0x01 + codec(1) + seq u32 BE + dur u16 BE
#      + flags u16 BE + i16le mono PCM; ~320 samples per frame at 16 kHz,
#      real-time paced so server-side VAD observes natural timing
#   5. send {"type":"stop","payload":{"reason":"user"}} (FIFO finalize barrier)
#      → exactly one terminal `final` (partials may arrive before it)
#
# This runs REAL INFERENCE on the daemon (CPU) — it is a functional demo, not a
# stub. Expect it to take as long as the WAV is long, plus decode time. Use a
# short 16 kHz mono 16-bit PCM WAV. A repo fixture that works:
#   core/crates/vad-rs/tests/fixtures/dots.wav
#
# Requirements: python3 + `pip install websockets` (v13+ verified).
#
# Run:
#   export VOXIX_TOKEN=vx_…
#   python3 examples/api/python/sessions_stream.py ./speech.wav
#
# Expected output: session banner, live partials on one line, then exactly one
#   "final:" line with the transcript.
# Exit codes: 0 final received · 1 transport/protocol/service failure · 2 usage
import asyncio
import json
import os
import struct
import sys
import urllib.error
import urllib.request
import wave

try:
    from websockets.asyncio.client import connect
except ImportError:
    sys.exit(
        "error: the 'websockets' package is required for this example\n"
        "       → pip install websockets   (v13+; this repo does not install it)\n"
        "       The other api/python examples are stdlib-only; this one speaks a binary WebSocket.\n"
        "       If you cannot install it, use ../../sdk/05-streaming.ts instead."
    )

BASE = os.environ.get("VOXIX_BASE_URL", "http://127.0.0.1:47832").rstrip("/")
TOKEN = os.environ.get("VOXIX_TOKEN")
if not TOKEN:
    sys.exit("error: VOXIX_TOKEN is not set")
if len(sys.argv) != 2:
    sys.exit("usage: sessions_stream.py <file.wav>   (16 kHz mono 16-bit PCM)")
WAV = sys.argv[1]

SAMPLE_RATE = 16_000
FRAME_MS = 20
SAMPLES_PER_FRAME = SAMPLE_RATE * FRAME_MS // 1000  # 320


def ws_url(base, path):
    return base.replace("http://", "ws://", 1).replace("https://", "wss://", 1) + path


def load_wav(path):
    try:
        with wave.open(path, "rb") as w:
            channels, sampwidth, rate = w.getnchannels(), w.getsampwidth(), w.getframerate()
            raw = w.readframes(w.getnframes())
    except wave.Error as e:
        sys.exit(f"error: cannot decode {path} as a PCM WAV ({e})")
    if rate != SAMPLE_RATE:
        sys.exit(f"error: input is {rate} Hz — the v1 audio profile is 16 kHz mono s16le; resample first")
    if channels != 1:
        sys.exit(f"error: input has {channels} channels — the v1 audio profile is mono; downmix first")
    if sampwidth != 2:
        sys.exit(f"error: input is {sampwidth * 8}-bit — the v1 audio profile is 16-bit signed PCM; convert first")
    return raw  # little-endian i16 samples, mono


def encode_frame(seq, pcm_bytes):
    header = b"VX" + bytes([1, 1])  # version 1, codec 1 = pcm_s16le
    header += struct.pack(">I", seq)          # u32 BE audio sequence, starts at 0
    header += struct.pack(">H", FRAME_MS)     # u16 BE duration
    header += struct.pack(">H", 0)            # u16 BE flags (bit 0 = discontinuity; unset)
    return header + pcm_bytes


def http_json(method, path, body=None):
    data = json.dumps(body).encode("utf-8") if body is not None else None
    req = urllib.request.Request(BASE + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return resp.status, resp.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except urllib.error.URLError as e:
        sys.exit(f"error: cannot reach {BASE} ({e.reason})")


pcm = load_wav(WAV)
print(f"decoded {len(pcm) // 2} samples ({len(pcm) // 2 // SAMPLE_RATE} ms) from {WAV}")

# --- create -------------------------------------------------------------------
# Server-side VAD auto-segments speech; partial_results surfaces `partial` events
# (final_only models emit exactly one `final`).
status, body = http_json("POST", "/v1/sessions", {"vad": {"mode": "server"}, "partial_results": True})
if status != 201:
    print(f"create failed: HTTP {status} {body}", file=sys.stderr)
    sys.exit(1)
created = json.loads(body)
session_id = created["session_id"]
session_token = created["session_token"]
print(f"session {session_id} · streaming_mode={created.get('streaming_mode')} · negotiated={json.dumps(created.get('negotiated_audio'))}")

finals = []


async def drain(ws):
    async for raw in ws:
        if isinstance(raw, bytes):
            continue  # the service sends no binary events
        try:
            ev = json.loads(raw)
        except json.JSONDecodeError:
            print("\n(non-JSON server frame ignored)", file=sys.stderr)
            continue
        etype = ev.get("type")
        p = ev.get("payload") or {}
        if etype == "hello":
            print(f"hello · protocol_version={p.get('protocol_version')} heartbeat_ms={p.get('heartbeat_ms')}")
        elif etype == "started":
            print(f"started · model_id={p.get('model_id')} backend={p.get('backend')} resumed={p.get('resumed')}")
        elif etype == "partial":
            sys.stdout.write(f"\r› {p.get('committed', '')}{p.get('tentative', '')}")
            sys.stdout.flush()
        elif etype == "final":
            finals.append(p)
            print(f"\n✓ final (#{len(finals)} of the 1 expected)")
            print(f"  text: {p.get('text')}")
            print(f"  duration_ms={p.get('duration_ms')} language={p.get('language')} model_id={p.get('model_id')}")
            print(f"  history_id={p.get('history_id')}")
        elif etype == "speech_start":
            sys.stdout.write("\n[speech_start] ")
            sys.stdout.flush()
        elif etype == "speech_end":
            sys.stdout.write("\n[speech_end] ")
            sys.stdout.flush()
        elif etype == "error":
            print(f"\n✗ server event {p.get('code')}: {p.get('message')} (retryable={p.get('retryable')})", file=sys.stderr)
        # audio_ack / pong / other events: observed and ignored


async def run():
    headers = {"Authorization": f"Bearer {TOKEN}", "X-Session-Token": session_token}
    url = ws_url(BASE, created["ws_url"])
    try:
        async with connect(url, additional_headers=headers, open_timeout=10) as ws:
            # hello barrier — the server speaks first; no start before hello.
            hello_raw = await asyncio.wait_for(ws.recv(), timeout=10)
            hello = json.loads(hello_raw)
            if hello.get("type") != "hello":
                print(f"protocol error: expected hello, got {hello.get('type')}", file=sys.stderr)
                sys.exit(1)
            print(f"hello · protocol_version={hello.get('payload', {}).get('protocol_version')}")
            # started barrier — NO AUDIO BEFORE THIS LINE.
            await ws.send(json.dumps({"type": "start", "payload": {}}))
            started_raw = await asyncio.wait_for(ws.recv(), timeout=10)
            started = json.loads(started_raw)
            if started.get("type") != "started":
                print(f"protocol error: expected started, got {started.get('type')}: {started}", file=sys.stderr)
                sys.exit(1)
            print(f"started · model_id={started.get('payload', {}).get('model_id')}")
            # Handshake done — from here on the drain task owns ws.recv().
            reader = asyncio.create_task(drain(ws))
            # Pump frames: zero-padded tail, real-time pacing (~20 ms per frame).
            seq = 0
            for off in range(0, len(pcm), SAMPLES_PER_FRAME * 2):
                chunk = pcm[off:off + SAMPLES_PER_FRAME * 2]
                if len(chunk) < SAMPLES_PER_FRAME * 2:
                    chunk = chunk + b"\x00" * (SAMPLES_PER_FRAME * 2 - len(chunk))
                await ws.send(encode_frame(seq, chunk))
                seq += 1
                await asyncio.sleep(FRAME_MS / 1000)
            print(f"\npumped {seq} frames (~{seq * FRAME_MS} ms of audio) — sending stop")
            await ws.send(json.dumps({"type": "stop", "payload": {"reason": "user"}}))
            # stop resolves after the server emits its single terminal `final`;
            # the server then closes 1000, which ends the drain loop.
            await asyncio.wait_for(reader, timeout=60)
    except (asyncio.TimeoutError, TimeoutError):
        print("\ntimed out waiting for the terminal final (is an inference model installed?)", file=sys.stderr)
        sys.exit(1)
    except Exception as e:  # transport/protocol failure with a readable message
        print(f"\nsession failed: {e}", file=sys.stderr)
        sys.exit(1)


asyncio.run(run())
if len(finals) != 1:
    print(f"✗ expected exactly one final, observed {len(finals)}", file=sys.stderr)
    sys.exit(1)
print("✓ exactly one final received")
