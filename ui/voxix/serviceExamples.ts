const PLACEHOLDER = "vx_YOUR_TOKEN";

export const curlExample = (token?: string): string =>
  `curl -X POST http://127.0.0.1:47832/v1/transcriptions \\
  -H "Authorization: Bearer ${token ?? PLACEHOLDER}" \\
  -H "Content-Type: application/octet-stream" \\
  --data-binary @speech.wav
# 202 {"job_id":"0198…","status_url":"/v1/transcriptions/0198…"}

curl http://127.0.0.1:47832/v1/transcriptions/JOB_ID \\
  -H "Authorization: Bearer ${token ?? PLACEHOLDER}"
# {"state":"completed","result":{"transcript":"hello world"}}`;

export const jsExample = (token?: string): string =>
  `const base = "http://127.0.0.1:47832";
const token = "${token ?? PLACEHOLDER}";
const wav = await fetch("speech.wav").then((r) => r.arrayBuffer());

const accepted = await fetch(base + "/v1/transcriptions", {
  method: "POST",
  headers: {
    Authorization: "Bearer " + token,
    "Content-Type": "application/octet-stream",
  },
  body: wav,
}).then((r) => r.json());

for (;;) {
  const s = await fetch(base + accepted.status_url, {
    headers: { Authorization: "Bearer " + token },
  }).then((r) => r.json());
  if (s.state === "completed") {
    console.log(s.result.transcript);
    break;
  }
  if (s.state === "failed") {
    throw new Error(s.error && s.error.message ? s.error.message : "transcription failed");
  }
  await new Promise((r) => setTimeout(r, 250));
}`;

export const pythonExample = (token?: string): string =>
  `import time, requests

base = "http://127.0.0.1:47832"
token = "${token ?? PLACEHOLDER}"
headers = {"Authorization": "Bearer " + token}

with open("speech.wav", "rb") as fh:
    r = requests.post(
        base + "/v1/transcriptions",
        headers={**headers, "Content-Type": "application/octet-stream"},
        data=fh,
    )

status_url = base + r.json()["status_url"]

while True:
    s = requests.get(status_url, headers=headers).json()
    if s["state"] == "completed":
        print(s["result"]["transcript"])
        break
    if s["state"] == "failed":
        raise RuntimeError(s.get("error", {}).get("message", "transcription failed"))
    time.sleep(0.25)`;

export const ROUTES_SUMMARY =
  "GET /alive (no auth)  ·  GET /v1/health  ·  GET /v1/status  ·  GET /v1/models  ·  POST /v1/transcriptions (WAV body, 202)  ·  GET/DELETE /v1/transcriptions/{job}  ·  POST /v1/auth/tokens  ·  GET /v1/history";
