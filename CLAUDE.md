# mori-ear — agent guide

For Claude / agents working in this repo. README.md is the human-facing entry point; this file is what an agent needs to navigate efficiently.

## What this is

A minimal Rust CLI: global hotkey → microphone capture → STT → Groq LLM cleanup → clipboard + Ctrl+V paste-back into the focused window. No GUI, no tray, no persistent state. The "ear" organ in the Mori universe — runs as an independent process so mori-desktop restarts don't kill voice input.

STT has two backends (config `backend`, default `auto`): **Groq Whisper API** (cloud, fast) and a **local whisper-server** (the shared `~/.mori/whisper-server.json` discovery contract — see `src/local_stt.rs`). `auto` prefers local whisper-server first (data stays on-device), falls back to Groq on local failure (when a key exists); `groq` / `local` force one. mori-ear is a read-only **Adopter** of the local server (never starts/writes it — that's the Starter's job, today mori-meeting-recorder). Cleanup stays Groq; offline (no key) → STT goes local + cleanup is skipped (raw output).

Beyond the hotkey daemon, mori-ear also exposes an **outbound transcription service** so it can be the Mori universe's single STT provider: `GET /` (ready gate) + `POST /inference` (multipart `file`=WAV → `{"text":...}`), bound to `127.0.0.1:<ephemeral>` and advertised via its own descriptor `~/.mori/mori-ear-server.json` (see `src/service.rs`). AgentOS / mori-desktop consume this as clients. This is **not** a GUI/tray (those stay forbidden) — it's a headless, governable HTTP endpoint.

**Two governance manifests — different systems, don't conflate:**

- `agentos-manifest.json` — **AgentOS** `AppManifest` v2 (`agentos-core/src/manifest.rs`). `kind: body-part`, `provides: [{ skill: "ear.transcribe", kind: "http-service", http_service: { descriptor_path: "~/.mori/mori-ear-server.json" } }]`. This is what makes mori-ear installable into **AgentOS** and governed by its broker: AgentOS's `whisper_client` reads the descriptor → verifies alive → forwards `/inference`. (`SkillKind::HttpService` and `whisper_client` are **implemented** in agentos-core — verified; don't let stale notes call them "phantom".)
- `manifest.json` — **mori-desktop** `BodyManifest` (BI-1 body registry, `body/manifest.rs`, `schema_version: 1`, `kind: local_service`). A *separate* registry that lets the **body (mori-desktop)** discover the organ. Different schema, different consumer.

**Two descriptors, two roles — never conflate:** ear *reads* the shared `~/.mori/whisper-server.json` as an **Adopter** (someone else's raw whisper-server), and *writes* its own `~/.mori/mori-ear-server.json` as the **provider** of its smart `/inference` service. mori-ear must never write/lock `whisper-server.json`.

Every transcription (hotkey, batch, and service paths) runs under a **watchdog** (`src/watchdog.rs`): `transcribe_timeout_secs` (default 90) caps one STT+cleanup; on timeout the future is dropped (reqwest connection closes) so a stuck transcription can never wedge the daemon. `ChildGuard` is in place to `kill()` any spawned ffmpeg/whisper-cli child the moment a future is cancelled — currently dormant (STT is pure HTTP, no child) but wired for the Phase-2 local path.

## Layout

```
manifest.json         — mori-desktop Body Interface manifest (BodyManifest / BI-1, schema_version 1, kind=local_service)
agentos-manifest.json — AgentOS AppManifest v2 (kind=body-part, provides ear.transcribe as http-service)
src/
  main.rs        — entry, config, hotkey thread, paste-back, service wiring, watchdog wrap
  audio.rs       — cpal capture + WAV encode
  stt.rs         — Groq Whisper multipart POST
  cleanup.rs     — Groq LLM繁中 cleanup
  local_stt.rs   — local whisper-server fallback client (Adopter of ~/.mori/whisper-server.json)
  service.rs     — outbound HTTP transcription service (GET / · POST /inference) + descriptor writer
  watchdog.rs    — per-transcription timeout guard (+ ChildGuard for future local children)
  multipart.rs   — minimal multipart/form-data parser for POST /inference
scripts/
  install-autostart.ps1  — Windows scheduled task (self-elevating)
  remove-autostart.ps1   — delegates to install -Remove
  install-autostart.sh   — Linux XDG autostart entry
  restart.sh             — dev hot-reload: build → kill → relaunch
.github/workflows/
  build.yml      — Linux + Windows release builds; tag v* publishes a Release
```

## Build / run

```sh
cargo build                       # dev
cargo build --release             # release (Windows: windows_subsystem = "windows", no console)
cargo install --path .            # install to ~/.cargo/bin

# Linux dev loop
bash scripts/restart.sh           # build debug + relaunch (logs to /tmp/mori-ear.{out,err})
bash scripts/restart.sh --release # cargo install --path . then relaunch ~/.cargo/bin/mori-ear

# Windows: same cargo install --path .; no auto-restart helper script (kill manually in PowerShell)
```

No test suite yet. End-to-end verification is "real human + real microphone" — see README usage section.

## Platform-specific gotchas (have bitten us)

### Windows: global-hotkey requires a manual Win32 message pump

`global-hotkey 0.6` on Windows binds `RegisterHotKey` to a hidden tool window but does **not** spawn an internal message pump. The caller must run `GetMessage` / `DispatchMessage` on the same thread that created the manager — `tokio::time::sleep` does NOT pump Win32 messages. Symptom of forgetting this: `register` reports success, the "ready" log prints, but hotkey events never reach `GlobalHotKeyEvent::receiver()`.

`spawn_hotkey_thread` in `src/main.rs` is the fix: dedicated OS thread owns the manager, registers the hotkey, then loops on `GetMessageW`. Linux X11 is unaffected because the crate's X11 impl spawns its own event thread.

If you ever change the hotkey loop, do NOT collapse it back onto the tokio runtime.

### Windows: `windows_subsystem = "windows"` in release builds

Release builds drop the console subsystem (eliminates the black flash at scheduled-task logon). `AttachConsole(ATTACH_PARENT_PROCESS)` reattaches to a terminal if launched manually so logs still surface; the call silently no-ops for autostart. Don't remove either of those two pieces in isolation — they're a pair.

### Windows: install-autostart.ps1 self-elevates

`Register-ScheduledTask` / `Unregister-ScheduledTask` under `\` root require admin. The script detects non-elevated invocation and re-launches itself via `Start-Process -Verb RunAs`, passing `-OriginalUser` so the registered principal is the **calling** user (not whatever admin account UAC may pick to run the elevated shell). If you touch this script, preserve the OriginalUser passing — the alternative is registering the task for the wrong account on managed machines.

### Linux: single-instance + restart timing

X11 `XGrabKey` is per-client. Two mori-ear instances → second silently fails to grab. `single-instance` crate (abstract Unix socket on Linux, named mutex on Windows) ensures one process. `scripts/restart.sh` uses `pkill -9` + a poll loop because the abstract socket needs a beat after process death to be released.

### Linux: paste-back terminal detection

`paste-back_linux_clipboard` walks `xdotool getactivewindow getwindowpid` → `/proc/<pid>/comm` to decide between `Ctrl+V` and `Ctrl+Shift+V`. Same idea on Windows via `GetForegroundWindow` + `QueryFullProcessImageNameW`. If you add a terminal emulator, append to the lookup table in `needs_shift_for_paste*`.

## Config loading

Order (partial merge):

1. `~/.mori/ear.json` — full overrides (anything set wins)
2. `~/.mori/config.json` `providers.groq.api_key` — fallback for `groq_api_key` when ear.json doesn't set it (shared with mori-desktop)
3. `GROQ_API_KEY` env var — final fallback

Old behavior was "ear.json wins entirely or nothing" — a user writing a one-line ear.json to override hotkey lost the groq key and the process died. Don't regress this; the partial-merge logic in `Config::load` is small but load-bearing.

### STT backend selection (`backend`)

`ear.json` `backend` ∈ `auto` (default) | `groq` | `local`. Since `auto`/`local` can run without a Groq key, startup no longer hard-requires the key — it only bails when `backend = "groq"` and no key is resolvable. The local path reads `~/.mori/whisper-server.json`, verifies alive (loopback + `GET /` 200), **resamples to 16kHz mono** (whisper.cpp needs it; Groq resamples server-side so its path is untouched), then POSTs `/inference`. Security in `local_stt.rs` mirrors AgentOS whisper_client: loopback host pin, `redirect(none)`, `no_proxy()`, 512MB WAV cap.

### Transcription watchdog (`transcribe_timeout_secs`)

`ear.json` `transcribe_timeout_secs` (default `90`) is the **overall** cap on one STT(+cleanup), shared by the hotkey, batch, and service paths (`watchdog::guard`). It exists because the per-request reqwest timeouts (Groq 60s / local 120s / cleanup 30s) can stack to ~210s in the `auto` fallback chain — the watchdog gives one configurable ceiling. On timeout the future is dropped (connection closes) and that utterance is abandoned with a logged error; the daemon keeps running. Tune up if you batch genuinely long files.

### Outbound transcription service (`service.*`)

`ear.json` `service` controls the HTTP `/inference` provider:

```jsonc
{
  "service": {
    "enabled": true,   // false → only the hotkey daemon, no outbound endpoint
    "port": 0          // 0 = OS-assigned ephemeral; written into the descriptor
  }
}
```

When enabled, `run()` binds `127.0.0.1:<port>`, atomically writes `~/.mori/mori-ear-server.json`, and serves on a dedicated std thread (each request uses the tokio `Handle` to `block_on` the async transcribe — legal because that thread isn't a runtime worker). `POST /inference` accepts multipart `file` (WAV) plus optional `language` / `backend` (`auto`/`groq`/`local`, per-request override — this is how a caller forces on-device transcription) / `cleanup` (`false`/`0`/`raw`/`none` → skip the繁中 cleanup). On shutdown the `ServiceHandle` drop unblocks the server and removes the descriptor. Startup failure is non-fatal: it warns and the hotkey daemon keeps working.

## Audio guardrails (why STT might be skipped)

Two separate mechanisms — don't conflate them:

**(a) Whole-clip skip** — `handle_event` (Released branch) bails on:

- `duration_secs < 0.25` — hotkey press too brief, treated as a misfire
- `rms_db < -45.0` — recording is essentially silence, would trigger Whisper hallucinations ("謝謝", "請訂閱", etc.)

Tune in `src/main.rs` constants `MIN_DURATION` / `MIN_RMS_DB`. The thresholds intentionally trade off some "very quiet whisper" sensitivity for not pasting YouTube end-credit hallucinations. **These run on the pre-trim full-clip RMS/duration** (returned by `stop_and_encode_wav`), so trimming does NOT change skip behavior.

**(b) Silence trimming** — `audio.rs::apply_trim` cuts **leading/trailing silence (any length) + internal continuous pauses ≥ `min_silence_ms`** from the WAV *before* it's sent to STT (shorter upload, fewer edge hallucinations). Frame-RMS (20ms windows) over the downmixed mono, ported from mori-desktop's `recording.rs::trim_silence_runs`. Edges keep ~80ms padding so soft onsets aren't clipped; if the whole clip reads as silence, trimming is skipped and the full clip is sent (the whole-clip guard above then decides). Config-gated via `ear.json` `voice_input.*`:

```jsonc
{
  "voice_input": {
    "trim_silence_enabled": true,   // false → send full clip, old behavior
    "trim_silence_min_ms": 300,     // internal pause ≥ this is removed (clamped via serde defaults)
    "trim_silence_threshold": 0.02  // linear amplitude, 0.02 ≈ -34 dBFS — same defaults as mori-desktop
  }
}
```

Internal-pause removal is intentionally conservative (only ≥300ms) so Whisper keeps natural pauses as sentence-boundary cues. The skip gate still uses **average** RMS (not mori-desktop's peak-RMS-over-100ms-window upgrade) — a known limitation if you say a few words then leave a long silent tail.

## CI / release

- `build.yml` runs on every push + PR for both targets. Tag `v*` triggers the `release` job that publishes a GitHub Release with both artifacts.
- `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: "true"` is set so the v4 `actions/checkout` + `actions/upload-artifact` survive GitHub's Node 20 removal (2026-09-16). Annotation still appears until those actions get bumped to v5.

## What to NOT do

- Do not add a tray icon / GUI to mori-ear. That's mori-desktop's job.
- Do not add a feature toggle for "skip cleanup" to ear.json beyond the existing `raw` flag — the cleanup LLM is the difference between "謝謝你訂閱頻道" hallucinations and clean繁中.
- Do not introduce a tokio task that polls `GlobalHotKeyEvent::receiver()` on a tokio worker on Windows. See gotcha above.
- Do not auto-update `~/.mori/config.json` from mori-ear. That file is mori-desktop's source of truth; mori-ear is a read-only consumer of `providers.groq.api_key`.

## Linked organs

- [mori-desktop](https://github.com/yazelin/mori-desktop) — body / GUI / chat panel / annuli. Shared config dir, otherwise independent.
- Future: `mori-eye` (vision / screenshots), `mori-hand` (OS automation), `mori-lip` (TTS).
