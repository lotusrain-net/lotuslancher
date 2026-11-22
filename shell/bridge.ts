// The shell's local views talk to the backend through the `shell_*` commands
// and the `shell://` events only (capabilities/default.json).
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

export type Screen = "starting" | "init" | "offline" | "daily" | "quitting";

export type Status = {
  state?: string;
  connection?: string;
  transport?: string;
  transport_error?: string;
  transport_failure_since?: string | null;
  last_presence_at?: string | null;
  presence_failure_since?: string | null;
  authorization?: string;
  error?: string;
  driver_ready?: boolean;
  session_active?: boolean;
} | null;

export type Snapshot = {
  screen: Screen;
  drawer: boolean;
  maximized?: boolean;
  status: Status;
  engine_ready: boolean;
  engine_installing: boolean;
  engine_error: string | null;
  shell_version: string;
  autostart_available: boolean;
};

export type Prefs = {
  autostart: boolean;
  autostart_available: boolean;
  use_system_proxy_for_control: boolean;
};

export type Progress = { percent?: number } | null;

const HAS_BRIDGE = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Dev server in a plain browser only: `?screen=daily&drawer=1` draws that state. */
function devMock<T>(cmd: string): Promise<T> {
  const q = new URLSearchParams(location.search);
  const snap: Snapshot = {
    screen: (q.get("screen") as Screen) || "daily",
    drawer: q.get("drawer") === "1" || location.hash === "#drawer",
    status: q.get("problem") ? { connection: "offline", transport: "disconnected", presence_failure_since: new Date().toISOString() } : { state: "ready", connection: "online", transport: "connected", last_presence_at: new Date().toISOString() },
    engine_ready: true,
    engine_installing: q.get("screen") === "init",
    engine_error: null,
    shell_version: "1.2.11",
    autostart_available: true,
  };
  const prefs: Prefs = { autostart: true, autostart_available: true, use_system_proxy_for_control: false };
  return Promise.resolve((cmd === "shell_snapshot" ? snap : cmd === "shell_preferences" ? prefs : null) as T);
}

export function invoke<T = unknown>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!HAS_BRIDGE && import.meta.env.DEV) return devMock<T>(cmd);
  if (!HAS_BRIDGE) return Promise.reject("desktop_bridge_unavailable");
  return tauriInvoke<T>(cmd, args);
}

export function useEvent<T>(event: string, cb: (payload: T) => void) {
  useEffect(() => {
    if (!HAS_BRIDGE) return;
    let off: (() => void) | undefined;
    let gone = false;
    tauriListen<T>(event, (e) => cb(e.payload)).then((u) => (gone ? u() : (off = u)), () => {});
    return () => {
      gone = true;
      off?.();
    };
    // The callback is read through state setters; subscribing once is enough.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [event]);
}

const MESSAGES: Record<string, string> = {
  session_active: "请先在左侧结束当前会话后再清理",
  logout_in_progress: "正在退出登录，请稍后再试",
  agent_unreachable: "本机服务未响应，请稍后再试",
  agent_not_running: "本机服务未运行，请重启客户端",
  autostart_unavailable: "当前系统不支持开机自动启动",
  support_export_failed: "诊断日志包导出失败",
  action_not_allowed: "不支持的操作",
};
export const say = (code: unknown) => {
  const c = typeof code === "string" ? code : "operation_failed";
  return MESSAGES[c] ?? `操作失败（${c}）`;
};

export type Problem = { kind: string; text: string; action: [label: string, name: string] };

const CONNECTION_GRACE_MS = 90_000;
const AUTH_ERRORS = new Set([
  "authorization_rejected", "invalid_grant", "device_revoked",
  "device_seat_revoked", "device_seat_suspended", "device_ownership_conflict", "device_quota_exceeded",
  "device_restore_conflict", "seat_revoked", "seat_suspended",
]);
const AUTH_MESSAGES: Record<string, string> = {
  authorization_rejected: "LotusHub 授权已失效",
  invalid_grant: "LotusHub 授权已失效",
  device_revoked: "LotusHub 已撤销此设备授权",
  device_seat_revoked: "设备已被管理员吊销",
  device_seat_suspended: "设备已解绑，请重新登录",
  device_ownership_conflict: "设备已绑定其他账号",
  device_quota_exceeded: "可用设备数已达上限",
  device_restore_conflict: "设备仍有未清理会话",
  seat_revoked: "设备已吊销",
  seat_suspended: "设备已暂停",
};
const NETWORK_ERRORS = new Set(["control_unreachable", "timeout", "dns_failure", "tcp_reset", "tls_failure", "http_4xx", "http_5xx", "upgrade_rejected"]);

function networkWarningDue(status: NonNullable<Status>, now: number) {
  const reference = status.transport_error
    ? status.transport_failure_since
    : status.presence_failure_since ?? status.last_presence_at;
  if (!reference) return false;
  const at = Date.parse(reference);
  return Number.isFinite(at) && now - at >= CONNECTION_GRACE_MS;
}

/** Only problems requiring attention light the gear's dot. */
export function problem(s: Snapshot | null, now = Date.now()): Problem | null {
  const st = s?.status;
  if (!st || st.state === "unavailable" || !st.connection) return { kind: "agent", text: "本机服务未就绪", action: ["立即重连", "reconnect"] };
  const err = st.error || "";
  if (st.authorization === "reauth_required" || AUTH_ERRORS.has(err)) {
    return { kind: "authorization", text: AUTH_MESSAGES[err] ?? "LotusHub 授权失效", action: ["重新连接", "reconnect"] };
  }
  if (err === "proxy_auth_failed") {
    return { kind: "proxy", text: "系统代理认证失败", action: ["立即重连", "reconnect"] };
  }
  if (s.engine_error || /^driver_download_/.test(err) || err === "driver_launcher_checksum_mismatch") {
    return { kind: "download", text: "组件下载慢", action: ["换个线路重试", "driver_retry"] };
  }
  if (s.engine_ready && st.driver_ready === false || err.startsWith("driver_") || err.startsWith("engine_")) {
    return { kind: "component", text: "组件异常", action: ["一键修复", "repair"] };
  }
  if (err === "control_protocol_mismatch" || err === "command_trust_not_configured" || err === "command_key_mismatch") {
    return { kind: "service", text: "LotusHub 服务配置需要处理", action: ["重新连接", "reconnect"] };
  }
  const transportIssue = !!st.transport_error && (NETWORK_ERRORS.has(st.transport_error) || st.transport_error.startsWith("control_closed_"));
  const connectionIssue = st.connection !== "online" || transportIssue || NETWORK_ERRORS.has(err) || err.startsWith("control_closed_") || err.startsWith("control_http_");
  if (connectionIssue) {
    if (networkWarningDue(st, now)) {
      return { kind: "network", text: "暂时无法连接 LotusHub", action: ["立即重连", "reconnect"] };
    }
    return null;
  }
  if (err) return { kind: "component", text: "组件异常", action: ["一键修复", "repair"] };
  return null;
}

export function presenceTime(value?: string | null) {
  if (!value) return "尚未成功连接";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "尚未成功连接";
  return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" }).format(date);
}

export function act(name: string, snap: Snapshot | null) {
  if (name === "repair") return invoke("shell_action", { name: "probe" }).then(() => invoke("shell_action", { name: "retry_cleanup" }));
  if (name === "driver_retry" && snap && !snap.engine_ready) {
    return invoke("shell_action", { name: "driver_retry" }).then(() => invoke("shell_install_engine"));
  }
  return invoke("shell_action", { name });
}

/** The backend's picture, kept fresh by its events and a 2 s poll. */
export function useSnapshot() {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [progress, setProgress] = useState<Progress>(null);
  const refresh = () => invoke<Snapshot>("shell_snapshot").then(setSnap, () => {});
  useEvent<Snapshot>("shell://screen", setSnap);
  useEvent<Snapshot>("shell://status", setSnap);
  useEvent<Progress>("runtime:progress", setProgress);
  useEvent<null>("runtime:done", () => {
    setProgress(null);
    void refresh();
  });
  useEffect(() => {
    void refresh();
    const t = setInterval(refresh, 2000);
    return () => clearInterval(t);
  }, []);
  return { snap, progress, refresh };
}

export const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
