# mori-ear — agent guide

For Claude / agents working in this repo. README.md is the human-facing entry point; this file is what an agent needs to navigate efficiently.

## What this is

A minimal Rust CLI: global hotkey → microphone capture → STT → Groq LLM cleanup → clipboard + Ctrl+V paste-back into the focused window. The hotkey has **two sources** that both collapse into one internal `KeyEdge` (`Pressed`/`Released`) before reaching `handle_event`: `global-hotkey` (X11 / Windows) and the `GlobalShortcuts` portal (`src/wayland_hotkey.rs`, Wayland). There is **no toggle mode** — this organ is hold-only; the thing with `ToggleMode` is mori-desktop's `hotkeys.toggle_mode`, don't conflate them. No GUI, no tray, no persistent state. The "ear" organ in the Mori universe — runs as an independent process so mori-desktop restarts don't kill voice input.

STT has two backends (config `backend`, default `auto`): **Groq Whisper API** (cloud, fast) and a **local whisper-server** (the shared `~/.mori/whisper-server.json` discovery contract — see `src/local_stt.rs`). `auto` prefers local whisper-server first (data stays on-device), falls back to Groq on local failure (when a key exists); `groq` / `local` force one. mori-ear is an **Adopter** of the local server: it never *writes or locks* the descriptor (the Starter/Owner — today mori-meeting-recorder's `mori-whisper-serve` supervisor — does that). When the local server is offline, mori-ear may **request an on-demand start** via the contract §11 idempotent entry `~/.mori/bin/mori-whisper-serve --ensure` (it kicks the supervisor, which is the real Starter), then polls ≤15s for ready — see `wake_and_wait` in `src/local_stt.rs`. standalone-first: no supervisor / wake times out → fall back to Groq (`auto`) or error (`local`). Cleanup stays Groq; offline (no key) → STT goes local + cleanup is skipped (raw output).

Beyond the hotkey daemon, mori-ear also exposes an **outbound transcription service** so it can be the Mori universe's single STT provider: `GET /` (ready gate) + `POST /inference` (multipart `file`=WAV → `{"text":...}`), bound to `127.0.0.1:<ephemeral>` and advertised via its own descriptor `~/.mori/mori-ear-server.json` (see `src/service.rs`). AgentOS / mori-desktop consume this as clients. This is **not** a GUI/tray (those stay forbidden) — it's a headless, governable HTTP endpoint.

**Two governance manifests — different systems, don't conflate:**

- `agentos-manifest.json` — **AgentOS** `AppManifest` v2 (`agentos-core/src/manifest.rs`). `kind: body-part`, `provides: [{ skill: "ear.transcribe", kind: "http-service", http_service: { descriptor_path: "~/.mori/mori-ear-server.json" } }]`. This is what makes mori-ear installable into **AgentOS** and governed by its broker: AgentOS's `whisper_client` reads the descriptor → verifies alive → forwards `/inference`. (`SkillKind::HttpService` and `whisper_client` are **implemented** in agentos-core — verified; don't let stale notes call them "phantom".)
- `manifest.json` — **mori-desktop** `BodyManifest` (BI-1 body registry, `body/manifest.rs`, `schema_version: 1`, `kind: local_service`). A *separate* registry that lets the **body (mori-desktop)** discover the organ. Different schema, different consumer.

**Two descriptors, two roles — never conflate:** ear *reads* the shared `~/.mori/whisper-server.json` as an **Adopter** (someone else's raw whisper-server), and *writes* its own `~/.mori/mori-ear-server.json` as the **provider** of its smart `/inference` service. mori-ear must never write/lock `whisper-server.json` — it may only *request* a start via `mori-whisper-serve --ensure` (the supervisor does the writing/locking, contract §11).

Every transcription (hotkey, batch, and service paths) runs under a **watchdog** (`src/watchdog.rs`): `transcribe_timeout_secs` (default 90) caps one STT+cleanup; on timeout the future is dropped (reqwest connection closes) so a stuck transcription can never wedge the daemon. `ChildGuard` is in place to `kill()` any spawned ffmpeg/whisper-cli child the moment a future is cancelled — currently dormant (STT is pure HTTP, no child) but wired for the Phase-2 local path.

## Layout

```
manifest.json         — mori-desktop Body Interface manifest (BodyManifest / BI-1, schema_version 1, kind=local_service)
agentos-manifest.json — AgentOS AppManifest v2 (kind=body-part, provides ear.transcribe as http-service)
src/
  main.rs        — entry, config, KeyEdge fan-in, hotkey thread, paste-back, service wiring, watchdog wrap
  wayland_hotkey.rs — GlobalShortcuts portal hotkey (Wayland; Activated/Deactivated → KeyEdge)
  audio.rs       — cpal capture + WAV encode
  stt.rs         — Groq Whisper multipart POST
  cleanup.rs     — Groq LLM繁中 cleanup
  local_stt.rs   — local whisper-server fallback client (Adopter of ~/.mori/whisper-server.json)
  service.rs     — outbound HTTP transcription service (GET / · POST /inference) + descriptor writer
  watchdog.rs    — per-transcription timeout guard (+ ChildGuard for future local children)
  multipart.rs   — minimal multipart/form-data parser for POST /inference
scripts/
  ear.sh                 — Linux one-shot wrapper (install/status/deps/keybind/autostart/toggle)
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

### Linux Wayland: XGrabKey is silently dead, use the portal

`global-hotkey` grabs via X11. Under GNOME Wayland mori-ear is an XWayland client, and the compositor only routes keys into XWayland **while an X11 window has focus** — the moment focus lands on a Wayland-native window the grab receives nothing. Failure mode is identical to the Windows message-pump bug: `register` succeeds, "ready" logs, hotkey never fires.

`src/wayland_hotkey.rs` binds `org.freedesktop.portal.GlobalShortcuts` instead. Three things there are load-bearing:

1. **`register_host_app` + a `.desktop` file are mandatory.** Without a registered app id GNOME rejects `CreateSession` with `NotAllowed: An app id is required`; without `~/.local/share/applications/<APP_ID>.desktop` the registration itself fails with `App info not found`. mori-desktop hit this first — see its `crates/mori-tauri/src/portal_hotkey.rs`.
2. **`APP_ID` must differ from mori-desktop's** (`ai.yazelin.mori-ear` vs `ai.yazelin.mori`). Portal permissions are keyed by app id; sharing one would make the two organs overwrite each other's grant.
3. **`Handle` must be held for the daemon's lifetime.** Dropping the `Session` closes it and the binding dies — same discipline as `_service` in `run()`.

`preferred_trigger` is only a hint: once the user grants permission the compositor owns the binding, so editing `ear.json` `hotkey` afterwards does nothing. That's why `spawn` logs `actual=` — it's the only way a user can tell what's really bound. Reset by deleting `~/.local/share/xdg-desktop-portal/permissions`.

Do NOT run both sources at once. `run()` only starts the X11 bridge when the portal path didn't take, because an XGrabKey registration under Wayland "succeeds" while being permanently dead — two live sources would just make diagnosis harder.

### Linux Wayland: paste-back can't detect the terminal

X11 walks `xdotool getactivewindow` → `/proc/<pid>/comm` to pick Ctrl+V vs Ctrl+Shift+V. Wayland deliberately denies clients any focused-window query (GNOME 45+ closed the Shell `Eval` escape hatch too), so that detection is **impossible** here — hence the `paste_key` config field. Default `ctrl+v`; terminal users must set `ctrl+shift+v` themselves. Don't try to reintroduce auto-detection without a working mechanism.

`paste_back_wayland` uses `wl-copy` + `ydotool` (virtual keyboard via `/dev/uinput`, so Wayland-native windows accept it). It needs `ydotoold` running and the user in the `input` group; on failure it falls back to the X11 path, which still helps hybrid setups (mori-desktop forces `GDK_BACKEND=x11`, so its windows are XWayland).

### Linux: paste-back deps are session-dependent, and `ear.sh` is where that's checked

The binary can install cleanly, autostart fine, and the hotkey can fire — and paste-back still does nothing, because it shells out to external tools that differ per session: X11 needs `xclip` + `xdotool`, Wayland needs `wl-clipboard` + `ydotool` **plus** a running `ydotoold` and the user in the `input` group (which only takes effect after a re-login). Nothing in the Rust code can fix a missing package, so `check_deps` in `scripts/ear.sh` reports it at install time instead of letting the user discover it mid-sentence.

Missing deps are deliberately **non-fatal** to `ear install` — stdout still carries the transcript, so the daemon is useful without paste-back. If you add a new external command to any paste-back path, add it to `check_deps` in the matching session branch, or it becomes another silent failure.

### Linux: `pre_exec_close_fds` masks exec failures as SIGABRT

The hook closes every fd >= 3 in the forked child. Rust std reports exec failure to the parent through a CLOEXEC pipe whose fd is also >= 3 — so the hook closes it too. When `exec` then fails (typically ENOENT), the child cannot write the errno back, `rtassert!` fires, and the process `abort()`s. The parent sees `signal: 6 (SIGABRT) (core dumped)` for what is really "file not found".

This burned us with `mori-whisper-serve --ensure`: a machine that simply doesn't have mori-meeting-recorder installed produced a core-dump message on every single transcription, which reads like the supervisor crashed. `request_wake` now checks `program.is_file()` before spawning, so the common case reports honestly (and skips a doomed fork/exec).

Any *new* spawn that goes through `pre_exec_close_fds` inherits this trap. Check the binary exists first, or expect a misleading SIGABRT.

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

### `paste_key` (Wayland only)

`ear.json` `paste_key` (default `ctrl+v`) is the chord `paste_back_wayland` injects. It exists purely because Wayland can't tell us whether the focused window is a terminal (see the gotcha above). X11 / Windows ignore it — they auto-detect. Accepted tokens are in `ydotool_keycodes`; they map to **Linux input event codes**, not ASCII.

### STT backend selection (`backend`)

`ear.json` `backend` ∈ `auto` (default) | `groq` | `local`. Since `auto`/`local` can run without a Groq key, startup no longer hard-requires the key — it only bails when `backend = "groq"` and no key is resolvable. The local path reads `~/.mori/whisper-server.json`, verifies alive (loopback + `GET /` 200); **if offline it requests an on-demand wake** via `mori-whisper-serve --ensure` and polls ≤15s (`WAKE_READY_TIMEOUT_SECS`) for ready (contract §11 — mori-ear stays an Adopter, never writes/locks the descriptor). Then it **resamples to 16kHz mono** (whisper.cpp needs it; Groq resamples server-side so its path is untouched) and POSTs `/inference`. Security in `local_stt.rs` mirrors AgentOS whisper_client: loopback host pin, `redirect(none)`, `no_proxy()`, 512MB WAV cap. The `--ensure` spawn goes through `pre_exec_close_fds` (Linux) so the short-lived shim never carries mori-ear's single-instance socket.

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
