// The shell's local views: `#drawer` is the settings drawer, anything else
// the main view (title bar, or a full-window page). Plain script, no bundler:
// it talks to the shell through Tauri's IPC and the `shell_*` commands only.
(function () {
  "use strict";
  var T = window.__TAURI_INTERNALS__;
  var isDrawer = location.hash === "#drawer";
  var snap = null;
  var prefs = null;
  var progress = null;

  function invoke(cmd, args) {
    if (!T) return Promise.reject("desktop_bridge_unavailable");
    return T.invoke(cmd, args || {});
  }
  function listen(event, cb) {
    if (!T || !T.transformCallback) return;
    invoke("plugin:event|listen", { event: event, target: { kind: "Any" }, handler: T.transformCallback(function (e) { cb(e.payload); }) }).catch(function () {});
  }
  function $(sel, root) { return (root || document).querySelector(sel); }
  function $$(sel, root) { return Array.prototype.slice.call((root || document).querySelectorAll(sel)); }
  function btn(label, onClick, cls) {
    var b = document.createElement("button");
    b.textContent = label;
    if (cls) b.className = cls;
    b.addEventListener("click", onClick);
    return b;
  }

  var MESSAGES = {
    session_active: "请先在左侧结束当前会话后再清理",
    logout_in_progress: "正在退出登录，请稍后再试",
    agent_unreachable: "本机服务未响应，请稍后再试",
    agent_not_running: "本机服务未运行，请重启客户端",
    autostart_unavailable: "当前系统不支持开机自动启动",
    support_export_failed: "诊断日志包导出失败",
    action_not_allowed: "不支持的操作",
  };
  function say(code) { return MESSAGES[code] || "操作失败（" + code + "）"; }

  // ---- what is wrong, if anything ----
  function problem(s) {
    var st = s && s.status;
    if (!st || st.state === "unavailable" || !st.connection) return { kind: "agent", text: "本机服务未就绪", action: ["立即重连", "reconnect"] };
    if (st.connection === "offline" || st.connection === "reconnecting") return { kind: "network", text: "无法连接服务器", action: ["立即重连", "reconnect"] };
    var err = st.error || "";
    if (s.engine_error || /^driver_download_/.test(err) || err === "driver_launcher_checksum_mismatch") {
      return { kind: "download", text: "组件下载慢", action: ["换个线路重试", "driver_retry"] };
    }
    if (err || (s.engine_ready && st.driver_ready === false)) return { kind: "component", text: "组件异常", action: ["一键修复", "repair"] };
    return null;
  }

  function act(name) {
    if (name === "repair") {
      return invoke("shell_action", { name: "probe" }).then(function () { return invoke("shell_action", { name: "retry_cleanup" }); });
    }
    if (name === "driver_retry" && snap && !snap.engine_ready) {
      return invoke("shell_action", { name: "driver_retry" }).then(function () { return invoke("shell_install_engine"); });
    }
    return invoke("shell_action", { name: name });
  }

  function toast(text, root) {
    var el = $("[data-toast]", root || document);
    if (!el) return;
    el.textContent = text;
    el.hidden = false;
    clearTimeout(toast.t);
    toast.t = setTimeout(function () { el.hidden = true; }, 3000);
  }

  function run(button, p, ok, root) {
    if (button) button.disabled = true;
    return p.then(function (r) {
      if (ok) toast(typeof ok === "function" ? ok(r) : ok, root);
    }, function (e) {
      toast(say(typeof e === "string" ? e : "operation_failed"), root);
    }).then(function () {
      if (button) button.disabled = false;
      refresh();
    });
  }

  // ---- settings cards (drawer, or beside a full-window page) ----
  function mountSettings(host, onClose) {
    host.innerHTML = "";
    host.appendChild($("#settings-tpl").content.cloneNode(true));
    $("[data-close-settings]", host).addEventListener("click", onClose);
    $$("[data-act]", host).forEach(function (b) {
      var name = b.getAttribute("data-act");
      b.addEventListener("click", function () {
        run(b, invoke("shell_action", { name: name }), name === "collect_logs" ? "诊断日志包已导出" : null, host);
      });
    });
    $("[data-copy-summary]", host).addEventListener("click", function (ev) {
      var b = ev.currentTarget;
      run(b, invoke("shell_summary").then(copy), "已复制", host);
    });
    $("[data-cleanup]", host).addEventListener("click", function (ev) {
      run(ev.currentTarget, invoke("shell_cleanup"), "临时数据已清理", host);
    });
    $("[data-uninstall]", host).addEventListener("click", function (ev) {
      if (!window.confirm("确定卸载 LotusProxy 客户端？卸载程序会先清理本机数据。")) return;
      run(ev.currentTarget, invoke("shell_action", { name: "remove" }), null, host);
    });
    $("[data-pref=autostart]", host).addEventListener("change", function (ev) {
      var on = ev.currentTarget.checked;
      run(null, invoke("shell_set_autostart", { on: on }).then(loadPrefs), null, host);
    });
    $("[data-pref=system_proxy]", host).addEventListener("change", function (ev) {
      var on = ev.currentTarget.checked;
      run(null, invoke("shell_set_system_proxy", { on: on }).then(loadPrefs), null, host);
    });
    loadPrefs();
    renderSettings(host);
  }

  function copy(text) {
    if (navigator.clipboard && navigator.clipboard.writeText) return navigator.clipboard.writeText(text);
    var ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
    return Promise.resolve();
  }

  function loadPrefs() {
    return invoke("shell_preferences").then(function (p) { prefs = p; render(); }, function () {});
  }

  function renderSettings(host) {
    if (!host || !host.firstChild) return;
    var p = problem(snap);
    var st = (snap && snap.status) || {};
    $("[data-status-dot]", host).className = "status-dot " + (p ? "bad" : "ok");
    $("[data-status-text]", host).textContent = p ? p.text : "一切正常";
    var actions = $("[data-status-actions]", host);
    actions.innerHTML = "";
    if (p) {
      actions.appendChild(btn(p.action[0], function (ev) { run(ev.currentTarget, act(p.action[1]), "已重试", host); }));
    }
    var auto = $("[data-pref=autostart]", host);
    var proxy = $("[data-pref=system_proxy]", host);
    var hint = $("[data-pref-hint]", host);
    if (prefs) {
      auto.checked = !!prefs.autostart;
      auto.disabled = !prefs.autostart_available;
      proxy.checked = !!prefs.use_system_proxy_for_control;
      hint.hidden = !!prefs.autostart_available;
      hint.textContent = "开机自动启动仅在 Windows 上可用";
    }
    var active = !!st.session_active;
    $("[data-cleanup]", host).disabled = active;
    $("[data-cleanup-hint]", host).hidden = !active;
    $("[data-version]", host).textContent = (snap && snap.shell_version) || "-";
  }

  // ---- main view ----
  var PAGES = {
    starting: function () {
      return { spinner: true, title: "正在启动", text: "正在连接 LotusHub…" };
    },
    init: function (s) {
      var installing = s.engine_installing;
      var failed = !!s.engine_error;
      return {
        spinner: installing,
        title: "首次使用，正在准备浏览器组件",
        text: failed ? "组件下载失败，请检查网络后重试。" : installing ? "下载完成后会自动进入，不需要任何操作。" : "组件已就绪，正在进入…",
        progress: installing,
        actions: failed ? [["换个线路重试", function (ev) { run(ev.currentTarget, act("driver_retry")); }], ["打开设置", openSettings, "secondary"]] : [["打开设置", openSettings, "secondary"]],
      };
    },
    offline: function () {
      return {
        title: "无法连接 LotusHub",
        text: "请检查网络连接，或在设置中查看诊断信息。",
        actions: [["重新连接", function (ev) { run(ev.currentTarget, invoke("shell_reconnect")); }], ["打开设置", openSettings, "secondary"]],
      };
    },
    quitting: function () {
      return { spinner: true, title: "正在安全清理临时浏览器数据…", text: "完成后客户端会自动退出，请勿强制关闭。" };
    },
  };

  var pageSettingsOpen = false;
  function openSettings() {
    if (snap && snap.screen === "daily") invoke("shell_set_drawer", { open: true });
    else {
      pageSettingsOpen = true;
      var host = $("#page-settings");
      mountSettings(host, function () { pageSettingsOpen = false; render(); });
      render();
    }
  }

  function renderMain() {
    var s = snap || { screen: "starting" };
    var daily = s.screen === "daily";
    $("#titlebar").hidden = !daily;
    $("#page").hidden = daily;
    if (daily) {
      pageSettingsOpen = false;
      $("#gear-dot").hidden = !problem(s);
      $("#gear").classList.toggle("on", !!s.drawer);
      return;
    }
    var spec = (PAGES[s.screen] || PAGES.starting)(s);
    $("#page-spinner").hidden = !spec.spinner;
    $("#page-title").textContent = spec.title;
    $("#page-text").textContent = spec.text;
    var prog = $("#page-progress");
    prog.hidden = !spec.progress || !progress;
    if (progress) $("#page-bar").style.width = Math.max(2, Math.min(100, progress.percent || 0)) + "%";
    var actions = $("#page-actions");
    actions.innerHTML = "";
    (spec.actions || []).forEach(function (a) { actions.appendChild(btn(a[0], a[1], a[2])); });
    var settings = $("#page-settings");
    var showSettings = pageSettingsOpen && s.screen !== "quitting";
    settings.hidden = !showSettings;
    if (showSettings) renderSettings(settings);
  }

  function render() {
    if (isDrawer) renderSettings($("#drawer"));
    else renderMain();
  }

  function refresh() {
    return invoke("shell_snapshot").then(function (s) { snap = s; render(); }, function () {});
  }

  // ---- wiring ----
  document.addEventListener("keydown", function (e) {
    if (e.key !== "Escape") return;
    if (isDrawer) invoke("shell_set_drawer", { open: false });
    else if (pageSettingsOpen) { pageSettingsOpen = false; render(); }
  });
  $$("[data-win]").forEach(function (b) {
    b.addEventListener("click", function () { invoke("shell_window", { op: b.getAttribute("data-win") }); });
  });
  $$("[data-drag]").forEach(function (el) {
    el.addEventListener("mousedown", function (e) {
      if (e.button !== 0 || e.target.closest("button")) return;
      invoke("shell_window", { op: e.detail === 2 ? "toggle_maximize" : "drag" });
    });
  });
  $("#gear").addEventListener("click", function () {
    invoke("shell_set_drawer", { open: !(snap && snap.drawer) });
  });
  document.addEventListener("contextmenu", function (e) { e.preventDefault(); });

  if (isDrawer) {
    var drawer = $("#drawer");
    drawer.hidden = false;
    document.body.classList.add("is-drawer");
    mountSettings(drawer, function () { invoke("shell_set_drawer", { open: false }); });
  }

  listen("shell://screen", function (s) { snap = s; render(); if (isDrawer && s.drawer) loadPrefs(); });
  listen("shell://status", function (s) { snap = s; render(); });
  listen("shell://open-settings", function () { if (!isDrawer) openSettings(); });
  listen("runtime:progress", function (p) { progress = p; render(); });
  listen("runtime:done", function () { progress = null; refresh(); });
  setInterval(refresh, 2000);
  refresh();
})();
