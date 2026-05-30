//! mori-ear 對外轉譯服務 — 讓「耳朵」開一個受治理的 HTTP 轉譯端點,給 AgentOS
//! (http-service skill)/ mori-desktop 當 client 消費。
//!
//! 端點(對齊 whisper-server-contract.md 的 baseline 形狀):
//!   GET  /            → 200「mori-ear ok」 (契約 §3.1 的 authoritative ready gate,client 驗活用)
//!   GET  /health      → 200(同上,別名)
//!   POST /inference   → multipart/form-data:`file`=WAV(必),`language`/`backend`/`cleanup`(選)
//!                       → STT(+繁中 cleanup)經看門狗 → 200 {"text":"..."};失敗 = 非 2xx + 文字 body
//!
//! 跟 ear 既有「Adopter of `~/.mori/whisper-server.json`(共享 raw whisper-server)」是兩回事:
//!   - 那邊 ear 當 **client**,用別人(Starter)起的 raw server(`local_stt.rs`)。
//!   - 這邊 ear 當 **provider**,自己開的「智慧」轉譯服務(內部再選 backend = 共享 server / Groq,
//!     並做繁中 cleanup)。descriptor 另寫 `~/.mori/mori-ear-server.json`,**絕不碰** whisper-server.json
//!     (角色分離:不搶 whisper-server 的 lock、不寫它的 descriptor)。
//!
//! 執行緒模型:tiny_http 是 blocking accept loop,獨立一條 std thread 跑(像 hotkey thread)。
//! 每個 request 用傳入的 tokio `Handle::block_on` 跑 async 轉譯 —— 這條 thread 不是 runtime
//! worker(沒有 tokio context),block_on 合法不 panic。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::{cleanup, multipart, watchdog};

/// 服務啟動時要的參數(從 `Config` 攤平成 owned,丟進 service thread)。
pub struct ServiceParams {
    pub backend: String,
    pub api_key: Option<String>,
    pub language: String,
    pub cleanup_prompt_file: String,
    /// 預設是否跳過 cleanup(= `Config::raw`);per-request `cleanup` 欄位可覆寫。
    pub skip_cleanup_default: bool,
    /// 一次轉譯(STT+cleanup)的整體上限。
    pub timeout: Duration,
}

/// 服務 handle —— 綁住 daemon 生命週期。drop 時收掉 server + 刪 descriptor。
pub struct ServiceHandle {
    server: Arc<Server>,
    descriptor: std::path::PathBuf,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        self.server.unblock();
        // best-effort 刪自己的 descriptor(別讓 stale 檔誤導 client 以為服務還在)。
        let _ = std::fs::remove_file(&self.descriptor);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// 啟動服務:bind 127.0.0.1:<port>(0 = ephemeral)→ 原子寫 descriptor → spawn accept thread。
pub fn serve(handle: tokio::runtime::Handle, params: ServiceParams, port: u16) -> Result<ServiceHandle> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| anyhow::anyhow!("bind {addr} 失敗: {e}"))?;
    let server = Arc::new(server);
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);

    let descriptor = write_descriptor(bound_port).context("寫 mori-ear-server descriptor")?;
    tracing::info!(
        port = bound_port,
        descriptor = %descriptor.display(),
        "mori-ear 轉譯服務上線(GET / 驗活、POST /inference 轉錄)"
    );

    let srv = server.clone();
    let params = Arc::new(params);
    let join = std::thread::Builder::new()
        .name("mori-ear-service".into())
        .spawn(move || {
            for req in srv.incoming_requests() {
                handle_request(req, &handle, &params);
            }
        })
        .context("spawn service thread")?;

    Ok(ServiceHandle {
        server,
        descriptor,
        join: Some(join),
    })
}

fn handle_request(req: Request, handle: &tokio::runtime::Handle, params: &ServiceParams) {
    let method = req.method().clone();
    let url = req.url().to_string();
    let path = url.split('?').next().unwrap_or(&url).to_string();

    match (&method, path.as_str()) {
        (Method::Get, "/") | (Method::Get, "/health") => {
            let _ = req.respond(Response::from_string("mori-ear ok"));
        }
        (Method::Post, "/inference") => respond_inference(req, handle, params),
        _ => {
            let _ = req.respond(Response::from_string("not found").with_status_code(404));
        }
    }
}

/// 讀 body → 解析 multipart → 經看門狗轉譯 → 單一回應點(成功 200 {"text"} / 失敗 4xx·5xx 文字)。
/// `Request::respond` 吃 owned self,所以這裡 own `req`、最後只回應一次。
fn respond_inference(mut req: Request, handle: &tokio::runtime::Handle, params: &ServiceParams) {
    let content_type = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();

    let mut body = Vec::new();
    let read_res = req.as_reader().read_to_end(&mut body);

    // 算出結果(Ok=轉錄文字 / Err=(status, 訊息)),全程不碰 req,最後一次回應。
    let outcome: std::result::Result<String, (u16, String)> = (|| {
        read_res.map_err(|e| (400u16, format!("讀 request body 失敗: {e}")))?;
        let boundary = multipart::boundary_of(&content_type)
            .ok_or((400u16, "需 multipart/form-data(帶 boundary)".to_string()))?;
        let form = multipart::parse(&boundary, &body);
        let wav = form
            .file()
            .map(|b| b.to_vec())
            .ok_or((400u16, "缺 'file' part(WAV bytes)".to_string()))?;

        let language = form
            .text("language")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| params.language.clone());
        let backend = form
            .text("backend")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| params.backend.clone());
        // cleanup 欄位:false/0/raw/none → 跳過 cleanup;有給其他值 → 做;沒給 → 用預設。
        let skip_cleanup = match form.text("cleanup").as_deref() {
            Some("false") | Some("0") | Some("raw") | Some("none") => true,
            Some(_) => false,
            None => params.skip_cleanup_default,
        };

        let api_key = params.api_key.clone();
        let prompt_file = params.cleanup_prompt_file.clone();
        let timeout = params.timeout;

        let job = async move {
            watchdog::guard(timeout, "service:/inference", async move {
                let raw =
                    crate::transcribe_with_fallback(&backend, api_key.as_deref(), &language, wav)
                        .await?;
                if skip_cleanup {
                    return Ok::<String, anyhow::Error>(raw);
                }
                match api_key.as_deref() {
                    Some(key) => {
                        let prompt = cleanup::resolve_system_prompt(&prompt_file);
                        let cleaned = match cleanup::cleanup(key, &raw, &prompt).await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(error = ?e, "service cleanup 失敗,用 raw");
                                raw
                            }
                        };
                        Ok(cleaned)
                    }
                    None => Ok(raw),
                }
            })
            .await
        };

        // service thread 非 runtime worker → block_on 合法。
        handle
            .block_on(job)
            .map_err(|e| (500u16, format!("轉譯失敗: {e}")))
    })();

    let resp = match outcome {
        Ok(text) => Response::from_string(serde_json::json!({ "text": text }).to_string())
            .with_header(json_header()),
        Err((code, msg)) => Response::from_string(msg).with_status_code(code),
    };
    let _ = req.respond(resp);
}

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static header always valid")
}

/// ear 服務的 descriptor 路徑。
fn descriptor_path() -> std::path::PathBuf {
    crate::home_dir().join(".mori").join("mori-ear-server.json")
}

/// 是否已有「在線的」mori-ear 轉譯服務(讀自家 descriptor → loopback pin → `GET /` 200)。
/// `--serve` 用它讓位(已有就不重複起);desktop 端的 lazy-spawn 也是先驗活再決定要不要拉。
pub async fn existing_service_alive() -> bool {
    let Ok(text) = std::fs::read_to_string(descriptor_path()) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let host = v.get("host").and_then(|h| h.as_str()).unwrap_or_default();
    let port = v.get("port").and_then(|p| p.as_u64()).unwrap_or(0);
    if !(host == "127.0.0.1" || host == "::1") || port == 0 {
        return false;
    }
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    matches!(
        client.get(format!("http://{host}:{port}/")).send().await,
        Ok(r) if r.status().is_success()
    )
}

/// 寫 ear 服務的發現檔 `~/.mori/mori-ear-server.json`(契約 schema,原子寫)。
fn write_descriptor(port: u16) -> Result<std::path::PathBuf> {
    let dir = crate::home_dir().join(".mori");
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("mori-ear-server.json");
    let tmp = dir.join("mori-ear-server.json.tmp");
    let body = serde_json::json!({
        "contract_version": 1,
        "host": "127.0.0.1",
        "port": port,
        // 服務是動態 backend(共享 server / Groq),非單一模型 → 標 informational。
        "model": "mori-ear/auto",
        "pid": std::process::id(),
        "started_at": rfc3339_utc_now(),
        "inference_path": "/inference",
    });
    std::fs::write(&tmp, serde_json::to_vec_pretty(&body)?).context("write descriptor tmp")?;
    std::fs::rename(&tmp, &path).context("rename descriptor")?;
    Ok(path)
}

/// SystemTime → RFC3339 UTC(尾綴 Z)。契約 §8 要求 writer 輸出 Z。不引 chrono(守極簡),
/// 用 Howard Hinnant 的 days->civil 換算。
fn rfc3339_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days(Hinnant),epoch 1970-01-01。
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}
