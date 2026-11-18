// REFERENCE-ONLY — not a shipped library. No Gradle wiring, no test harness,
// no claim of build compatibility. It documents the mapping an Android
// (OkHttp) capture host performs on top of protocol v1 + the §6 binary frame:
//
//   POST /v1/sessions            bearer token            → 201 {session_id, session_token, ws_url, ...}
//   GET  {ws_url}                WS upgrade, custom headers:
//                                  Authorization: Bearer vx_…
//                                  X-Session-Token: <session_token>
//                                (OkHttp can attach custom headers; WebSocket
//                                runtimes that cannot should use the one-use
//                                ticket fallback instead: POST /v1/auth/ws-ticket,
//                                then open ?ticket=… — the ticket substitutes
//                                the bearer ONLY, X-Session-Token still applies)
//   {"type":"start","payload":{}}                                     → started barrier
//   {"type":"ping","payload":{"nonce":N}}                             → pong (keep-alive)
//   {"type":"pause"/"resume","payload":{}}                            → paused / (no resumed event)
//   {"type":"stop","payload":{"reason":"user"}}                       → one final event, then close 1000
//   {"type":"cancel","payload":{"reason":"user"}}                     → results suppressed, close 1000
//   binary       12-byte header "VX"+1+1 | BE u32 seq | BE u16 ms | BE u16 flags | LE PCM16 samples
//
// AudioWorklet-style capture on Android is an app concern: the equivalent
// shapes are AudioRecord(16_000, CHANNEL_IN_MONO, ENCODING_PCM_16BIT) reads
// sliced into 10/20/30/40/60 ms buffers.
package dev.voxix.example

import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okhttp3.Response
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

class VoxixOkHttpTranscriber(
  private val baseUrl: String,      // e.g. http://192.168.1.20:47832 (LAN pairing flow provisions the token)
  private val bearer: String,
  private val maxAudioSeconds: Int = 120,
) {
  private val http = OkHttpClient.Builder()
    .pingInterval(java.time.Duration.ofSeconds(10)) // WS-level keepalive alongside app-level ping
    .build()

  fun createSession(body: JSONObject = JSONObject()): JSONObject =
    http.newCall(
      Request.Builder()
        .url("$baseUrl/v1/sessions")
        .header("Authorization", "Bearer $bearer")
        .post(okhttp3.RequestBody.create(null, body.toString().toByteArray()))
        .build(),
    ).execute().use { response ->
      check(response.code == 201) { "session create failed: ${response.code}" }
      JSONObject(response.body!!.string())
    }

  fun transcribe(
    // Real capture feeds Int16Array chunks of 10/20/30/40/60 ms @16 kHz here.
    frames: Sequence<ShortArray>,
    frameMs: Int = 20,
  ) {
    val created = createSession()
    val sessionId = created.getString("session_id")
    val sessionToken = created.getString("session_token")
    val wsPath = created.getString("ws_url")

    val request = Request.Builder()
      .url(baseUrl.replace("http", "ws") + wsPath)
      .header("Authorization", "Bearer $bearer")
      .header("X-Session-Token", sessionToken)
      .build()

    val listener = object : WebSocketListener() {
      var seq = 0
      override fun onOpen(webSocket: WebSocket, response: Response) {
        webSocket.send("""{"type":"start","payload":{}}""")
        // hello arrives first; started is the barrier; audio follows.
        frames.forEach { chunk -> webSocket.send(frame(chunk, seq++, frameMs)) }
        webSocket.send("""{"type":"stop","payload":{"reason":"end_of_stream"}}""")
      }
      override fun onMessage(webSocket: WebSocket, text: String) {
        val event = JSONObject(text)
        when (event.getString("type")) {
          "partial" -> onPartial(event.getJSONObject("payload").optString("committed", "") + event.getJSONObject("payload").optString("tentative"))
          "final" -> onFinal(event.getJSONObject("payload").optString("text"))
          "error" -> onError(event.getJSONObject("payload").optString("code"))
        }
      }
      override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
        onError("transport: ${t.message}")
      }
    }
    http.newWebSocket(request, listener)
  }

  // §6 binary audio frame — mirrors packages/sdk/src/protocol/audio.ts.
  private fun frame(pcm: ShortArray, seq: Int, durationMs: Int, discontinuity: Boolean = false): okio.ByteString {
    val out = ByteArrayOutputStream(12 + pcm.size * 2)
    out.write("VX".toByteArray(Charsets.US_ASCII))
    out.write(1)
    out.write(1) // codec 1 = pcm_s16le
    ByteBuffer.allocate(4).order(ByteOrder.BIG_ENDIAN).putInt(seq).let { out.write(it.array()) }
    ByteBuffer.allocate(8).order(ByteOrder.BIG_ENDIAN)
      .putShort(durationMs.toShort())
      .putShort(if (discontinuity) 1 else 0)
      .let { out.write(it.array()) }
    val payload = ByteBuffer.allocate(pcm.size * 2).order(ByteOrder.LITTLE_ENDIAN)
    for (sample in pcm) payload.putShort(sample)
    out.write(payload.array())
    return okio.ByteString.of(*out.toByteArray())
  }

  private fun onPartial(draft: String) {}
  private fun onFinal(text: String) {}
  private fun onError(code: String) {}
}
