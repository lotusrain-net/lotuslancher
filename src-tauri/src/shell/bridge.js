// window.lotusDesktop: injected into the hub view on every document, defined
// only in the top frame of the control origin. Each method is one Tauri
// command the hub capability allows; nothing here holds a device token or a
// private key, and the driver API (127.0.0.1:40325) is never reached from here.
(function () {
  "use strict";
  var ORIGIN = __LOTUS_CONTROL_ORIGIN__;
  if (window.top !== window || location.origin !== ORIGIN || window.lotusDesktop) return;

  var internals = null;
  function invoke(cmd, args) {
    if (!internals) internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== "function") return Promise.reject(new Error("desktop_bridge_unavailable"));
    return internals.invoke(cmd, args || {});
  }
  function code(err) {
    var s = typeof err === "string" ? err : err && err.message;
    return /^[a-z0-9_]{1,80}$/.test(s || "") ? s : "operation_failed";
  }
  function fail(err) {
    var e = new Error(code(err));
    e.code = code(err);
    throw e;
  }
  function sleep(ms) {
    return new Promise(function (r) { setTimeout(r, ms); });
  }

  // ---- status: one poll loop, many subscribers ----
  var last = null;
  var listeners = [];
  var logoutAcked = false;
  var offlineTimer = null;
  var offlineReported = false;
  var connectivityRound = 0;
  function poll() {
    return invoke("desktop_status").then(function (s) {
      last = s;
      for (var i = 0; i < listeners.length; i++) {
        try { listeners[i](s); } catch (e) {}
      }
      if (s && s.logout_stage === "webview_clear" && !logoutAcked) {
        logoutAcked = true;
        endIamSession().then(function () { return invoke("desktop_hub_logout_ready"); }).catch(function () {});
      }
      if (!s || s.logout_stage !== "webview_clear") logoutAcked = false;
      return s;
    }, function () { return last; });
  }
  setInterval(poll, 2000);

  // Stage 4 of a desktop logout: end the IAM session, at most 3 s, before the
  // shell wipes this view's storage (it does so anyway if this never answers).
  function endIamSession() {
    var ctl = typeof AbortController === "function" ? new AbortController() : null;
    var timer = setTimeout(function () { if (ctl) ctl.abort(); }, 3000);
    return fetch("/api/v1/auth/logout", {
      method: "POST",
      credentials: "include",
      headers: { "content-type": "application/json" },
      body: "{}",
      signal: ctl ? ctl.signal : undefined,
    }).catch(function () {}).then(function () { clearTimeout(timer); });
  }

  // ---- device context assertion, refreshed every 2 minutes ----
  var ctx = null;
  function getDeviceContext(opts) {
    var userId = opts && opts.userId;
    var now = Math.floor(Date.now() / 1000);
    if (ctx && ctx.userId === userId && ctx.issuedAt + 120 > now) return Promise.resolve(ctx.value);
    return invoke("desktop_device_context", { userId: userId }).then(function (v) {
      var value = { deviceId: v.device_id, assertion: v.assertion, expiresAt: v.exp };
      ctx = { userId: userId, issuedAt: v.iat, value: value };
      return value;
    }, fail);
  }

  // ---- automatic binding: challenge -> local signature -> grant -> claim ----
  // The page makes the two authenticated calls itself (its cookie, its API
  // client); the shell signs and hands the ticket to the agent.
  var binding = null;
  function bindCurrentDevice(opts) {
    if (binding) return binding;
    binding = (async function () {
      var userId = opts.userId;
      var challenge = await opts.requestChallenge();
      var signed = await invoke("desktop_sign_challenge", {
        challengeId: challenge.challenge_id,
        challengeNonce: challenge.challenge_nonce,
        userId: userId,
      }).catch(fail);
      var grant = await opts.submitGrant(signed);
      await invoke("desktop_claim_grant", { grantTicket: grant.grant_ticket, expiresAt: grant.expires_at }).catch(fail);
      // The agent claims the ticket in the background; wait for its answer.
      // The agent retries until the ticket's 120 s run out, then drops it.
      var started = Date.now();
      while (Date.now() - started < 130000) {
        var s = await poll();
        if (s && s.authorization === "authorized" && !s.pending_grant) {
          ctx = null;
          return s;
        }
        // The shell's status is up to 2 s old; past that, a dropped ticket
        // with no authorization means the claim failed for good.
        if (s && !s.pending_grant && s.authorization === "signed_out" && Date.now() - started > 5000) fail("desktop_grant_failed");
        await sleep(1000);
      }
      fail("desktop_grant_timeout");
    })().finally(function () { binding = null; });
    return binding;
  }

  var api = {
    version: 1,
    getStatus: function () { return last ? Promise.resolve(last) : poll(); },
    subscribe: function (cb) {
      if (typeof cb !== "function") return function () {};
      listeners.push(cb);
      if (last) { try { cb(last); } catch (e) {} } else poll();
      return function () { listeners = listeners.filter(function (x) { return x !== cb; }); };
    },
    getDeviceContext: getDeviceContext,
    bindCurrentDevice: bindCurrentDevice,
    logoutDesktop: function () { ctx = null; return invoke("desktop_logout").then(poll, fail); },
    cancelLogout: function () { return invoke("desktop_cancel_logout").then(poll, fail); },
    openSettings: function () { return invoke("desktop_open_settings").catch(fail); },
  };
  Object.freeze(api);
  Object.defineProperty(window, "lotusDesktop", { value: api, writable: false, configurable: false, enumerable: false });

  function ready() {
    connectivityRound += 1;
    if (offlineTimer !== null) clearTimeout(offlineTimer);
    offlineTimer = null;
    offlineReported = false;
    invoke("desktop_hub_ready").catch(function () {});
  }

  function probeHub() {
    var controller = new AbortController();
    var timer = setTimeout(function () { controller.abort(); }, 5000);
    return fetch(ORIGIN + "/", {
      method: "HEAD",
      credentials: "same-origin",
      cache: "no-store",
      signal: controller.signal,
    }).then(function () {
      clearTimeout(timer);
      return true;
    }, function () {
      clearTimeout(timer);
      return false;
    });
  }

  function confirmHubConnectivity() {
    var round = ++connectivityRound;
    if (offlineTimer !== null) clearTimeout(offlineTimer);
    offlineTimer = setTimeout(function () {
      offlineTimer = null;
      probeHub().then(function (reachable) {
        if (round !== connectivityRound) return;
        if (reachable) {
          if (offlineReported) {
            offlineReported = false;
            location.reload();
          } else {
            ready();
          }
          return;
        }
        if (!offlineReported) {
          offlineReported = true;
          invoke("desktop_hub_offline").catch(function () {});
        }
        offlineTimer = setTimeout(function () {
          offlineTimer = null;
          confirmHubConnectivity();
        }, 5000);
      });
    }, 2000);
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", ready, { once: true });
  else ready();
  window.addEventListener("offline", confirmHubConnectivity);
  window.addEventListener("online", confirmHubConnectivity);
  poll();
})();
