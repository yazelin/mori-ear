// =========== Theme toggle ===========
// 在 <head> 的 inline script 已經先讀 localStorage 設好 data-theme,避免 FOUC。
// 這裡只處理 click → 切換 + persist。沒設過 → 推算「目前 OS 偏好」當基準。
(function () {
  const root = document.documentElement;
  const btn = document.querySelector(".theme-toggle");
  if (!btn) return;

  function currentTheme() {
    const attr = root.getAttribute("data-theme");
    if (attr === "light" || attr === "dark") return attr;
    return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }

  btn.addEventListener("click", () => {
    const next = currentTheme() === "light" ? "dark" : "light";
    root.setAttribute("data-theme", next);
    try { localStorage.setItem("mori-ear-theme", next); } catch (_) {}
  });
})();

// =========== Copy buttons ===========
// 從 code block 上的 data-copy 屬性讀 plain-text payload,避免從 DOM 抓會帶 <span> token tags
document.querySelectorAll(".code-block").forEach((block) => {
  const btn = block.querySelector(".copy-btn");
  if (!btn) return;
  btn.addEventListener("click", async () => {
    const payload = block.dataset.copy ?? block.querySelector("pre")?.textContent ?? "";
    try {
      await navigator.clipboard.writeText(payload);
      const orig = btn.textContent;
      btn.textContent = "已複製";
      btn.classList.add("copied");
      setTimeout(() => {
        btn.textContent = orig;
        btn.classList.remove("copied");
      }, 1500);
    } catch {
      btn.textContent = "失敗";
      setTimeout(() => (btn.textContent = "複製"), 1500);
    }
  });
});
