// Alkahest website behaviour: tabs, copy buttons, scroll reveal, OS detection and the download
// links of the latest GitHub release. Everything degrades gracefully: without JavaScript, or when
// the GitHub API is unreachable, every download link points at the releases page.
(() => {
  "use strict";

  const REPO = "digiogithub/alchemai-alkahest";
  const lang = document.documentElement.lang || "en";
  const T = {
    en: { copy: "copy", copied: "copied", released: "released" },
    es: { copy: "copiar", copied: "copiado", released: "publicada" },
  }[lang.slice(0, 2)] || { copy: "copy", copied: "copied", released: "released" };

  document.documentElement.classList.add("js");

  // Tabs: a [data-tabs] group of [role=tab] buttons, each naming its panel in aria-controls.
  for (const group of document.querySelectorAll("[data-tabs]")) {
    const tabs = [...group.querySelectorAll('[role="tab"]')];
    const select = (tab) => {
      for (const t of tabs) {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        t.tabIndex = on ? 0 : -1;
        const panel = document.getElementById(t.getAttribute("aria-controls"));
        if (panel) panel.hidden = !on;
      }
    };
    tabs.forEach((tab, i) => {
      tab.addEventListener("click", () => select(tab));
      tab.addEventListener("keydown", (e) => {
        const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
        if (!step) return;
        const next = tabs[(i + step + tabs.length) % tabs.length];
        select(next);
        next.focus();
      });
    });
  }

  // Copy buttons copy the text of the element named in data-copy.
  for (const btn of document.querySelectorAll("[data-copy]")) {
    btn.textContent = T.copy;
    btn.addEventListener("click", async () => {
      const src = document.getElementById(btn.dataset.copy);
      if (!src) return;
      try {
        await navigator.clipboard.writeText(src.innerText.trim());
        btn.textContent = T.copied;
        btn.classList.add("done");
        setTimeout(() => {
          btn.textContent = T.copy;
          btn.classList.remove("done");
        }, 1600);
      } catch {
        /* clipboard blocked: the text stays selectable */
      }
    });
  }

  // Reveal sections as they scroll into view.
  const reveals = document.querySelectorAll(".reveal");
  if ("IntersectionObserver" in window) {
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            e.target.classList.add("in");
            io.unobserve(e.target);
          }
        }
      },
      { rootMargin: "0px 0px -8% 0px" },
    );
    reveals.forEach((el) => io.observe(el));
  } else {
    reveals.forEach((el) => el.classList.add("in"));
  }

  // Highlight the visitor's operating system.
  const ua = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.userAgent || "";
  const os = /win/i.test(ua) ? "windows" : /mac|iphone|ipad/i.test(ua) ? "macos" : /linux|x11|android/i.test(ua) ? "linux" : "";
  if (os) document.querySelector(`.os[data-os="${os}"]`)?.classList.add("detected");

  // Fill the download links from the latest release. Asset names carry the version, so each link
  // names a pattern (data-asset) instead of a file.
  const PATTERNS = {
    appimage: /_x86_64\.AppImage$/,
    deb: /_amd64\.deb$/,
    rpm: /^alkahest-\d[^-]*-\d+\.x86_64\.rpm$/,
    "rpm-headless": /^alkahest-headless-.*\.x86_64\.rpm$/,
    "rpm-headless-arm": /^alkahest-headless-.*\.aarch64\.rpm$/,
    "tar-linux-x64": /x86_64-unknown-linux-gnu\.tar\.gz$/,
    "tar-linux-arm": /aarch64-unknown-linux-gnu\.tar\.gz$/,
    "dmg-arm": /_aarch64\.dmg$/,
    "dmg-x64": /_x64\.dmg$/,
    "tar-mac-arm": /aarch64-apple-darwin\.tar\.gz$/,
    "tar-mac-x64": /x86_64-apple-darwin\.tar\.gz$/,
    exe: /_x64-setup\.exe$/,
    zip: /windows-msvc\.zip$/,
    sums: /^SHA256SUMS$/,
  };
  const size = (b) => (b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.max(1, Math.round(b / (1 << 20)))} MB`);

  fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: "application/vnd.github+json" } })
    .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
    .then((rel) => {
      const version = String(rel.tag_name || "").replace(/^v/, "");
      for (const el of document.querySelectorAll("[data-version]")) el.textContent = `v${version}`;
      const date = rel.published_at ? new Date(rel.published_at) : null;
      if (date) {
        const fmt = date.toLocaleDateString(lang, { year: "numeric", month: "short", day: "numeric" });
        for (const el of document.querySelectorAll("[data-release-date]")) el.textContent = `${T.released} ${fmt}`;
      }
      for (const link of document.querySelectorAll("[data-asset]")) {
        const re = PATTERNS[link.dataset.asset];
        const asset = re && (rel.assets || []).find((a) => re.test(a.name));
        if (!asset) continue;
        link.href = asset.browser_download_url;
        link.title = asset.name;
        const out = link.querySelector("[data-size]");
        if (out) out.textContent = size(asset.size);
      }
    })
    .catch(() => {
      /* rate limited or offline: links keep pointing at the releases page */
    });
})();
