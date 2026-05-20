// 複製 button — 從 code block 上的 data-copy 屬性讀 plain-text payload,
// 避免從 DOM 抓會帶上 <span> token tags。
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
