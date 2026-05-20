# mori-ear GitHub Pages site

Static HTML served from `/docs/` on `main`. No Jekyll (see `.nojekyll`).

## Local preview

```sh
# any static server works
python3 -m http.server -d docs 8080
# 或 npx serve docs
```

打開 http://localhost:8080/

## 結構

```
docs/
  index.html          ← 主頁(繁中)
  styles.css          ← CSS,深紫底 + 淺紫 / 粉 / 青配色
  scripts.js          ← copy-to-clipboard
  favicon.svg
  .nojekyll           ← 關掉 Jekyll(我們直接出靜態 HTML)
  assets/
    mockups/
      page-mockup.png ← codex-imagegen 生的設計參考稿(不會被 index 引用)
```

## 設計來源

`assets/mockups/page-mockup.png` 是用 [codex-imagegen-skill](https://github.com/yazelin/codex-imagegen-skill) 跑 `$imagegen` 生出的 design draft。CSS 的配色 (`--bg #1a1428`、`--lavender`、`--blush`、`--sage`)、區塊順序(hero → quickstart → 安裝卡 → 使用流 → feature grid → support matrix → footer)都對齊那張圖。

要重生設計稿:

```sh
bash ~/Temp/codex-imagegen.sh \
  "<see prompt in git log for the docs commit>" \
  docs/assets/mockups/page-mockup.png
```

## 部署

GitHub Pages 設成從 `main` branch、`/docs` folder 出。一推 main 就會自動 build + serve。
