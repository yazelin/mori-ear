//! 懸浮預覽視窗 —— 按住熱鍵時顯示目前解出的文字。
//!
//! **為什麼這不算違反 CLAUDE.md 的「不加 GUI」**:mori-ear 自己沒有任何繪圖程式碼,
//! 這裡跟 `xclip` / `xdotool` 一樣是 spawn 外部程式(`yad`),只負責把文字餵過去。
//! 視窗歸 yad 管,mori-ear 少了它照樣跑(沒裝 yad → 預覽關掉,其餘行為不變)。
//! 這是 2026-08-26 yazelin 拍板的例外:語音輸入看不到進度會沒安全感。
//!
//! 兩種模式:
//! - `open_live()` — 無邊框、不搶焦點,單純顯示。按住熱鍵講話時用。
//! - `confirm()` — 有按鈕、吃 Enter/Esc。轉出來太長時用,等使用者確認。

use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Child, Command, Stdio};

// 這裡刻意**不做**「清空再重寫」。yad --listen 可以用 form feed 清空,但那需要
// 「送 \f、等 150ms、再送新內容」三步(30ms 太短會失效),中間那 150ms 是空白畫面,
// 實測看起來就是一直閃,而且偶爾殘留上一句。
//
// 分段預覽本來就只會往後加字,追加正是 --listen 的預設行為。不要再繞回去清空重寫。

/// yad 有沒有裝。沒裝就整個預覽功能靜默關掉,不影響轉錄。
pub fn available() -> bool {
    which("yad")
}

fn which(bin: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {bin}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 開著的即時預覽視窗。drop 時自動關掉。
pub struct Live {
    child: Child,
}

impl Live {
    /// 開一個無邊框、置頂、不搶焦點的視窗。`--no-focus` 很重要:搶了焦點,
    /// 待會 paste-back 會貼到這個視窗裡而不是你原本在打字的地方。
    pub fn open(title: &str) -> Result<Self> {
        let child = Command::new("yad")
            .args([
                "--text-info",
                "--listen",
                "--wrap",
                // 每一段追加成一行,講久了會超出視窗 —— 沒有這個就看不到第四行以後
                "--tail",
                "--vscroll-policy=never", // 捲軸不必畫出來,佔空間又沒人拉
                "--no-buttons",
                "--undecorated",
                "--on-top",
                "--no-focus",
                "--skip-taskbar",
                "--sticky",
                "--fontname=Sans 13",
                "--margins=8",
                "--width=760",
                "--height=170",
                "--center",
                &format!("--title={title}"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn yad(預覽視窗)")?;
        Ok(Self { child })
    }

    /// 追加一段文字。寫失敗多半是使用者手動關掉視窗了,不值得中斷轉錄。
    pub fn append(&mut self, text: &str) {
        let Some(stdin) = self.child.stdin.as_mut() else {
            return;
        };
        let _ = write!(stdin, "{text}\n");
        let _ = stdin.flush();
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 使用者在確認視窗做了什麼決定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 按了送出。帶回**視窗裡當下的文字** —— 使用者可能改過,不能貼原稿。
    Send(String),
    /// Esc 或按了丟棄。
    Discard,
}

/// yad 的退出碼:0 = 第一顆按鈕(送出),1 = 第二顆(丟棄),252 = Esc / 關視窗。
fn verdict_from(code: Option<i32>, stdout: String) -> Verdict {
    match code {
        // 內容被清空就別貼一個空字串出去
        Some(0) if !stdout.trim().is_empty() => Verdict::Send(stdout),
        _ => Verdict::Discard,
    }
}

/// 長句的多行編輯視窗,擋住直到使用者決定。只有關掉 live_paste 時會用到
/// (toggle 是邊講邊貼,要改直接在游標處改)。
///
/// **送出要用滑鼠點按鈕,Enter 是換行** —— 這是刻意的:進到這裡就是因為
/// 內容夠長、需要多行編輯,那 Enter 本來就該是換行。想要 Enter 直接送出
/// 的話得換成 `--entry`,但那是單行,長句很難讀。
///
/// `--listen` 千萬別加:它跟 `--editable` 互斥,加了文字改不動、Esc 也關不掉(實測)。
pub fn confirm(title: &str, text: &str) -> Result<Verdict> {
    let tmp = std::env::temp_dir().join(format!("mori-ear-confirm-{}.txt", std::process::id()));
    std::fs::write(&tmp, text).context("寫確認視窗的暫存檔")?;
    let out = Command::new("yad")
        .args([
            "--text-info",
            "--editable",
            "--wrap",
            "--on-top",
            "--center",
            "--width=780",
            "--height=300",
            "--fontname=Sans 12",
            &format!("--filename={}", tmp.display()),
            &format!("--title={title}"),
            "--button=送出:0",
            "--button=丟棄 (Esc):1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("spawn yad(確認視窗)");
    let _ = std::fs::remove_file(&tmp);
    let out = out?;
    // --editable 會把視窗裡當下的內容印到 stdout
    let edited = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(verdict_from(out.status.code(), edited))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_maps_yad_exit_codes() {
        assert_eq!(
            verdict_from(Some(0), "改過的字".into()),
            Verdict::Send("改過的字".into())
        );
        assert_eq!(verdict_from(Some(1), "x".into()), Verdict::Discard);
        // 使用者把內容清空後按送出 → 不要貼空字串
        assert_eq!(verdict_from(Some(0), "   ".into()), Verdict::Discard);
        // yad 用 252 表示 Esc / 直接關掉視窗
        assert_eq!(verdict_from(Some(252), "x".into()), Verdict::Discard);
        // 被 kill 掉 → 沒有退出碼,當成丟棄比較安全(不要擅自貼出去)
        assert_eq!(verdict_from(None, "x".into()), Verdict::Discard);
    }
}
