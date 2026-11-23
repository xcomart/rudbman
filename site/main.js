// Project page behaviour, shared by rulogman, rudbman and rudbgen: keep the
// three copies identical. Everything here is an enhancement; the page reads
// fine without it.
(function () {
  "use strict";

  // Release assets the page links to directly: an element with
  // data-asset="x86_64-unknown-linux-gnu.AppImage" gets the download URL of
  // the latest release's file that ends with that suffix.
  var latest = null;
  function linkAssets(release) {
    latest = release;
    document.querySelectorAll("[data-asset]").forEach(function (el) {
      var suffix = "-" + el.getAttribute("data-asset");
      (release.assets || []).some(function (asset) {
        if (asset.name.slice(-suffix.length) !== suffix) return false;
        el.href = asset.browser_download_url;
        return true;
      });
    });
  }

  // The visitor's platform and, where the browser says, CPU.
  var platform = ((navigator.userAgentData && navigator.userAgentData.platform) ||
    navigator.platform || navigator.userAgent || "").toLowerCase();
  var os = /mac|iphone|ipad/.test(platform) ? "macos"
    : /win/.test(platform) ? "windows"
    : /linux|x11/.test(platform) ? "linux" : null;
  var arm = /aarch64|arm64|armv8/.test((navigator.platform + " " + navigator.userAgent).toLowerCase());

  // The hero button follows the platform: the winget or Homebrew command
  // where the install panel shows one, otherwise the file itself, for the
  // visitor's CPU: the Windows installer or the Linux AppImage.
  var hero = document.querySelector("[data-download]");
  var label = hero && hero.querySelector("[data-label]");
  var files = {
    windows: ["pc-windows-msvc-setup.exe", "Download installer"],
    linux: ["unknown-linux-gnu.AppImage", "Download AppImage"]
  };
  function heroFile() {
    var file = files[os];
    hero.setAttribute("data-asset", (arm ? "aarch64-" : "x86_64-") + file[0]);
    label.textContent = file[1];
    if (latest) linkAssets(latest);
  }
  if (hero && label && os) {
    var panel = document.getElementById("panel-" + os);
    if (panel && panel.querySelector(".term") && os !== "linux") {
      hero.href = "#install";
      label.textContent = os === "windows" ? "Install with winget" : "Install with Homebrew";
    } else if (files[os]) {
      heroFile();
      // Chromium reports the CPU only on request; others keep the guess.
      if (navigator.userAgentData && navigator.userAgentData.getHighEntropyValues) {
        navigator.userAgentData.getHighEntropyValues(["architecture"]).then(function (v) {
          if (v.architecture) { arm = v.architecture === "arm"; heroFile(); }
        }).catch(function () {});
      }
    }
  }

  // The latest release: its tag on the download button, its files on the links.
  var repo = document.documentElement.getAttribute("data-repo");
  if (repo && window.fetch) {
    fetch("https://api.github.com/repos/" + repo + "/releases/latest")
      .then(function (r) { return r.ok ? r.json() : null; })
      .then(function (release) {
        if (!release || !release.tag_name) return;
        document.querySelectorAll("[data-version]").forEach(function (el) {
          el.textContent = release.tag_name;
          el.hidden = false;
        });
        linkAssets(release);
      })
      .catch(function () {});
  }

  // Install tabs, opened on the visitor's own platform.
  var tabs = Array.prototype.slice.call(document.querySelectorAll('.tabs [role="tab"]'));
  function select(tab) {
    tabs.forEach(function (t) {
      var on = t === tab;
      t.setAttribute("aria-selected", on ? "true" : "false");
      t.tabIndex = on ? 0 : -1;
      document.getElementById(t.getAttribute("aria-controls")).hidden = !on;
    });
  }
  tabs.forEach(function (tab, i) {
    tab.addEventListener("click", function () { select(tab); });
    tab.addEventListener("keydown", function (e) {
      var step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
      if (!step) return;
      var next = tabs[(i + step + tabs.length) % tabs.length];
      select(next);
      next.focus();
    });
  });
  var mine = os && document.getElementById("tab-" + os);
  if (mine) select(mine);

  // Copy the commands of a terminal block, without prompts or comments.
  document.querySelectorAll(".copy").forEach(function (button) {
    button.addEventListener("click", function () {
      var lines = button.parentNode.querySelectorAll(".c");
      var text = Array.prototype.map.call(lines, function (l) { return l.textContent; }).join("\n");
      if (!navigator.clipboard) return;
      navigator.clipboard.writeText(text).then(function () {
        button.textContent = "Copied";
        button.classList.add("done");
        setTimeout(function () {
          button.textContent = "Copy";
          button.classList.remove("done");
        }, 1600);
      });
    });
  });
})();
