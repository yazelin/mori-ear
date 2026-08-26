//! Wayland 全域熱鍵 —— 走 `org.freedesktop.portal.GlobalShortcuts`。
//!
//! # 為什麼需要這條路
//!
//! `global-hotkey` 在 Linux 走 X11 `XGrabKey`。GNOME Wayland 下 mori-ear 是
//! XWayland client,而 compositor **只在焦點停在 X11 視窗時**才把按鍵餵進
//! XWayland —— 焦點一落在 Wayland-native 視窗(GNOME Terminal / Files / 多數
//! GTK4 app),grab 就完全收不到事件。症狀跟 Windows 忘了 pump message 一樣惡劣:
//! `register` 回報成功、log 印 "ready",然後熱鍵永遠不響。
//!
//! # 為什麼是 portal 而不是 GNOME 自訂快捷鍵
//!
//! GNOME 自訂快捷鍵(`org.gnome.settings-daemon` custom-keybindings)只有「按下」
//! 語意 —— 它是「按到這組鍵就執行這行指令」,拿不到放開。它只適合 toggle
//! (按一下開、再按一下停),但 mori-ear 的 portal 路徑同時支援 toggle 與 hold。
//!
//! `GlobalShortcuts` portal 是唯一同時給得起兩端的介面:`Activated` 對應按下、
//! `Deactivated` 對應放開,一對一 map 到 [`crate::KeyEdge`]。hold 會使用兩種
//! edge；toggle 只消費 `Pressed`,把 `Released` 交給上層忽略。
//!
//! # 跟 mori-desktop 的關係
//!
//! 身體(mori-desktop)早就走過這條路 —— `crates/mori-tauri/src/portal_hotkey.rs`。
//! 這裡是同一套機制的極簡版:同樣要 `register_host_app` + `.desktop` 檔
//! (見 [`ensure_desktop_file`] 的註解,那是 GNOME 的硬性要求),但只綁一顆
//! shortcut、不 emit 事件、直接推 [`crate::KeyEdge`]。
//!
//! 兩者用**不同的 APP_ID**(`ai.yazelin.mori` vs `ai.yazelin.mori-ear`),portal
//! 權限才會各自獨立 —— 使用者可以只授權耳朵、不授權身體,反之亦然。
//!
//! # 運作前提
//!
//! 需要 compositor 實作這個 portal(GNOME 45+ / KDE Plasma 6+)。沒有的話
//! [`spawn`] 回 Err,呼叫端 fallback 回 X11 路徑(焦點在 XWayland 視窗時仍可用)。

use anyhow::{Context, Result};
use ashpd::{
    desktop::{
        global_shortcuts::{GlobalShortcuts, NewShortcut},
        CreateSessionOptions, Session,
    },
    register_host_app, AppID,
};
use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, info, warn};

use crate::KeyEdge;

/// 這顆 shortcut 在 portal 裡的 id(程式內部識別;使用者看到的是 description)。
const SHORTCUT_ID: &str = "talk";

/// 向 portal Registry 註冊的 app id。
///
/// **不能跟 mori-desktop 的 `ai.yazelin.mori` 相同** —— portal 權限是以 app id
/// 為 key 存的,共用會讓兩個器官的授權互相覆寫。
///
/// 連字號只允許出現在最後一段(ashpd `AppID` 的解析規則),`mori-ear` 剛好合法。
const APP_ID: &str = "ai.yazelin.mori-ear";

/// 綁定的存活憑證 —— drop 掉 portal session 就關,熱鍵失效。呼叫端要 hold 住,
/// 跟 `run()` 裡的 `_service` 同樣道理。
pub struct Handle {
    /// 明確保留:`Session` 的 Drop 會送 `Close`,提早 drop 等於自廢熱鍵。
    _session: Session<GlobalShortcuts>,
    /// 同上 —— proxy 活著 signal stream 才有來源。
    _portal: GlobalShortcuts,
}

/// 現在這個 session 是不是 Wayland。
///
/// `XDG_SESSION_TYPE` 由 logind 設,GNOME/KDE 都有;少數精簡 WM 沒設,所以再看
/// `WAYLAND_DISPLAY` 兜底(compositor 一定會設給 client)。
pub fn is_wayland() -> bool {
    std::env::var("XDG_SESSION_TYPE").is_ok_and(|v| v.eq_ignore_ascii_case("wayland"))
        || std::env::var("WAYLAND_DISPLAY").is_ok_and(|v| !v.is_empty())
}

/// 把 mori-ear 的熱鍵字串(`Ctrl+Alt+E`)轉成 XDG "shortcuts" spec 的 trigger 語法。
///
/// spec 的 modifier 全大寫(`CTRL` / `ALT` / `SHIFT` / `SUPER`),按鍵本身用 XKB
/// keysym 名 —— 字母要**小寫**(`e`;大寫 `E` 在 XKB 是 Shift+e,會綁錯)。
/// 認不得的 token 原樣傳下去讓 portal 自己判斷。
fn to_portal_trigger(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|part| match part.trim().to_lowercase().as_str() {
            "ctrl" | "control" => "CTRL".to_string(),
            "alt" => "ALT".to_string(),
            "shift" => "SHIFT".to_string(),
            "super" | "meta" | "win" | "cmd" => "SUPER".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 註冊 portal 熱鍵,把 Activated/Deactivated 餵進 `tx`。
///
/// 回傳的 [`Handle`] 必須留著 —— drop 等於解除綁定。
pub async fn spawn(hotkey: &str, tx: UnboundedSender<KeyEdge>) -> Result<Handle> {
    // GNOME 的 portal 後端在放行註冊前會去查這個 app id 的 .desktop 檔。
    // 非 flatpak 的 binary 沒人幫我們寫,得自己補(失敗不致命,先試再說)。
    if let Err(e) = ensure_desktop_file() {
        warn!(error = ?e, "寫 desktop entry 失敗 —— portal 可能拒絕註冊");
    }

    // 告訴 portal 我們是誰。沒這步 GNOME 會用
    // `org.freedesktop.portal.Error.NotAllowed: An app id is required` 擋掉
    // CreateSession(mori-desktop 踩過同一個坑,見 portal_hotkey.rs)。
    // flatpak 環境會從 sandbox manifest 繼承,ashpd 自動跳過這個呼叫。
    let app_id: AppID = APP_ID
        .parse()
        .context("APP_ID 不是合法的 reverse-DNS 識別碼")?;
    register_host_app(app_id)
        .await
        .context("向 xdg-desktop-portal Registry 註冊 app id 失敗")?;
    debug!(app_id = APP_ID, "✓ 已向 portal Registry 註冊");

    let portal = GlobalShortcuts::new().await.context(
        "連不上 org.freedesktop.portal.GlobalShortcuts —— 沒裝 xdg-desktop-portal-gnome?",
    )?;

    let session = portal
        .create_session(CreateSessionOptions::default())
        .await
        .context("portal create_session 失敗")?;

    let trigger = to_portal_trigger(hotkey);
    let shortcut = NewShortcut::new(SHORTCUT_ID, "mori-ear:語音輸入").preferred_trigger(&*trigger);

    // 第一次呼叫會跳 GNOME 授權對話框(「mori-ear 想註冊 Ctrl+Alt+E」)。
    // 使用者同意後綁定持久化在 portal permissions,之後啟動都是靜默的。
    //
    // 注意 portal 規範:`preferred_trigger` 只是「建議」,compositor 有權改綁別的鍵,
    // 使用者事後也能在系統設定改。而且**同意過之後改 ear.json 的 hotkey 不會生效** ——
    // 要去「設定 → 鍵盤 → 檢視及自訂快捷鍵」改,或刪掉
    // `~/.local/share/xdg-desktop-portal/permissions` 讓 mori-ear 重新註冊。
    // 所以下面一定要把實際綁到的鍵印出來,不然使用者按了沒反應會無從查起。
    let request = portal
        .bind_shortcuts(&session, &[shortcut], None, Default::default())
        .await
        .context("portal bind_shortcuts 請求失敗(第一次會跳授權對話框,要按同意)")?;
    let bound = request
        .response()
        .context("portal bind_shortcuts 被拒絕(使用者取消授權?)")?;

    match bound.shortcuts().iter().find(|s| s.id() == SHORTCUT_ID) {
        Some(s) => info!(
            requested = %trigger,
            actual = %s.trigger_description(),
            "✓ Wayland portal 熱鍵已綁定(語音輸入)",
        ),
        None => warn!(
            requested = %trigger,
            "portal 接受了請求但沒回這顆 shortcut —— 可能該組合已被佔用。\
             到「設定 → 鍵盤 → 檢視及自訂快捷鍵」看 mori-ear 實際綁到什麼"
        ),
    }

    let mut activated = portal
        .receive_activated()
        .await
        .context("subscribe Activated signal 失敗")?;
    let mut deactivated = portal
        .receive_deactivated()
        .await
        .context("subscribe Deactivated signal 失敗")?;

    // 兩條 signal stream 併成一條 KeyEdge 流。用 select! 而非兩條 task,
    // 是為了讓 tx 關閉(主 loop 結束)時兩邊一起收工。
    //
    // `held` 是**去彈跳**,不是防禦性寫法:GNOME 的 portal 會把鍵盤自動重複
    // 原樣轉成一連串 Activated —— 實測按住 15 秒會收到約 490 個(~30ms 一個,
    // 正好是 auto-repeat 頻率)。下游 handle_event 雖然有「已在錄音中就忽略」
    // 的守衛擋著、功能不受影響,但每個都會印一行 WARN,一次錄音就把 log 洗掉
    // (`ear log` 是 tail -30,結果全是雜訊、看不到轉錄結果)。
    //
    // 在源頭收斂成真正的邊緣事件,下游才拿得到「按下一次 = 一個 Pressed」的保證,
    // 也不必依賴各家 compositor 的重複行為一致。
    tokio::spawn(async move {
        let mut held = false;
        loop {
            let edge = tokio::select! {
                Some(ev) = activated.next() => {
                    if ev.shortcut_id() != SHORTCUT_ID {
                        debug!(id = %ev.shortcut_id(), "忽略非本 shortcut 的 Activated");
                        continue;
                    }
                    if held {
                        continue; // auto-repeat,不是新的按下
                    }
                    held = true;
                    KeyEdge::Pressed
                }
                Some(ev) = deactivated.next() => {
                    if ev.shortcut_id() != SHORTCUT_ID {
                        debug!(id = %ev.shortcut_id(), "忽略非本 shortcut 的 Deactivated");
                        continue;
                    }
                    if !held {
                        // 沒按下卻收到放開 —— 綁定期間就按著、或 compositor 補送。
                        // 往下傳會讓 handle_event 對著空的 recorder slot 做事,直接吞掉。
                        debug!("收到 Deactivated 但目前不是按下狀態,忽略");
                        continue;
                    }
                    held = false;
                    KeyEdge::Released
                }
                else => {
                    warn!("portal signal stream 結束 —— 熱鍵停止運作");
                    break;
                }
            };
            if tx.send(edge).is_err() {
                debug!("KeyEdge receiver 已關閉,portal listener 收工");
                break;
            }
        }
    });

    Ok(Handle {
        _session: session,
        _portal: portal,
    })
}

/// 確保 `~/.local/share/applications/<APP_ID>.desktop` 存在且指向現在這顆 binary。
///
/// 為什麼需要:xdg-desktop-portal-gnome 在放行 host-app 註冊前會用 app id 去查
/// desktop entry,查不到就回 `Could not register app ID: App info not found`。
/// 非 flatpak 的 binary 沒有人幫忙產,只能自己寫。
///
/// 冪等 —— 內容不同才寫,所以 binary 換位置(cargo install → ~/.cargo/bin)
/// 重跑一次就會更新。
///
/// `NoDisplay=true`:mori-ear 是 headless daemon,不該出現在應用程式選單裡
/// (這點跟 mori-desktop 不同 —— 身體有 GUI,要在選單露出)。
fn ensure_desktop_file() -> Result<()> {
    let home = std::env::var("HOME").context("HOME 沒設")?;
    let dir = std::path::PathBuf::from(home).join(".local/share/applications");
    std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;

    let exe = std::env::current_exe().context("取 current_exe 失敗")?;
    let exe_str = exe.to_str().context("current_exe 路徑不是合法 UTF-8")?;

    let path = dir.join(format!("{APP_ID}.desktop"));
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=mori-ear\n\
         Comment=Mori 的耳朵 — 熱鍵語音輸入,轉錄後貼進焦點視窗\n\
         Exec={exe_str}\n\
         Icon=audio-input-microphone\n\
         Categories=Utility;AudioVideo;\n\
         Terminal=false\n\
         NoDisplay=true\n",
    );

    let needs_write = std::fs::read_to_string(&path).map_or(true, |existing| existing != content);
    if needs_write {
        std::fs::write(&path, &content).with_context(|| format!("write {}", path.display()))?;
        info!(path = %path.display(), exec = exe_str, "寫入 portal 用的 desktop entry");
        // 新版 GNOME 立刻讀得到,舊版 portal 會 cache,踢一下(失敗無所謂)。
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&dir)
            .status();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_uses_uppercase_modifiers_and_lowercase_key() {
        assert_eq!(to_portal_trigger("Ctrl+Alt+E"), "CTRL+ALT+e");
        assert_eq!(to_portal_trigger("ctrl+shift+v"), "CTRL+SHIFT+v");
        assert_eq!(to_portal_trigger("Super+Space"), "SUPER+space");
    }

    #[test]
    fn trigger_tolerates_spaces() {
        assert_eq!(to_portal_trigger("Ctrl + Alt + Y"), "CTRL+ALT+y");
    }

    /// APP_ID 必須跟 mori-desktop 的分開,否則 portal 權限互相覆寫。
    #[test]
    fn app_id_is_valid_and_distinct_from_body() {
        assert!(
            APP_ID.parse::<AppID>().is_ok(),
            "APP_ID 必須是合法 reverse-DNS"
        );
        assert_ne!(APP_ID, "ai.yazelin.mori", "不能跟 mori-desktop 共用 app id");
    }
}
