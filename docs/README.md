# mori-ear GitHub Pages site

Static HTML served from `/docs/` on `main`. No Jekyll (see `.nojekyll`).

## Local preview

```sh
npx http-server docs -p 8765 -s
# 或:python3 -m http.server -d docs 8080
```

打開 http://localhost:8765/。`?theme=light` / `?theme=dark` 可強制看單一主題。

## 結構

```
docs/
  index.html          ← 主頁(繁中)
  styles.css          ← Mori brand tokens(forest dark + ivory light 雙主題)
  scripts.js          ← theme toggle + copy-to-clipboard
  .nojekyll           ← 關掉 Jekyll(直接出靜態 HTML)
  assets/
    mori-listening.png   ← Hero illustration(取自 mori-desktop 的 mori-recording sprite)
    mori-idle.png        ← 備用 sprite,目前沒用到
    mori-logo-badge.png  ← favicon + nav brand mark
    mockups/
      mori-ear-mockup.png   ← codex-imagegen 用 mori-brand.png 當 reference 生的設計稿
```

## 設計來源

整套配色 / 字體 / 角色都對齊 [mori-desktop 的 brand book](https://github.com/yazelin/mori-desktop/blob/main/docs/brand.html):

- **palette**:forest-night `#1f3329` / forest-deep `#2C4A3D` / forest `#6A8F72` / forest-soft `#A8C5A2` / cream `#E6DECA` / ivory `#F3F0E6` / sand `#B8A98E` / charcoal `#3B2F2F`
- **typography**:Heading = Noto Serif TC(森之筆 Serif)、Body = Noto Sans TC(森之筆 Sans)
- **mascot**:Mori(森林精靈 — 長綠髮、白花、漢服)用 `mori-recording` sprite 當「在聽」hero
- **dual theme**:dark = 森林夜、light = 林光紙,brand book 都有 spec

`assets/mockups/mori-ear-mockup.png` 是 [codex-imagegen-skill](https://github.com/yazelin/codex-imagegen-skill) 在 image-edit 模式餵 mori-desktop 的 `mori-brand.png` 當 reference 生出來的設計稿,當作 layout / mood 的視覺基準。實作的 HTML/CSS 對齊那張圖的氛圍但用真資料。

要重生 mockup:

```sh
bash ~/Temp/codex-imagegen.sh \
  "<see prompt in git log for the brand-pivot commit>" \
  docs/assets/mockups/mori-ear-mockup.png \
  ~/Temp/mori-brand-refs/mori-brand.png
```

## 部署

GitHub Pages source = `main` branch / `/docs` folder。推到 main 就會自動 build + serve 在 https://yazelin.github.io/mori-ear/。
