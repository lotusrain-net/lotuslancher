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
  error_source?: string;
  error_phase?: string;
  driver_ready?: boolean;
  session_active?: boolean;
  cleanup_pending?: boolean;
  cleanup_phase?: string;
  cleanup_state?: "none" | "active" | "closing" | "awaiting_server" | "isolated" | "complete" | "unknown";
} | null;

export type Snapshot = {
  screen: Screen;
  drawer: boolean;
  maximized?: boolean;
  status: Status;
  agent_health?: "checking" | "healthy" | "retrying" | "offline";
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

export type Progress = {
  percent?: number;
  route?: "system_proxy" | "direct";
  attempt?: number;
  max_attempts?: number;
} | null;

const HAS_BRIDGE = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Dev server in a plain browser only: `?screen=daily&drawer=1` draws that state. */
function devMock<T>(cmd: string): Promise<T> {
  const q = new URLSearchParams(location.search);
  const starting = q.get("screen") === "starting";
  const agentHealth = q.get("health") === "healthy" || !starting ? "healthy" : "checking";
  const snap: Snapshot = {
    screen: (q.get("screen") as Screen) || "daily",
    drawer: q.get("drawer") === "1" || location.hash === "#drawer",
    status: agentHealth === "checking" ? null : q.get("problem") ? { connection: "reconnecting", transport: "disconnected", presence_failure_since: new Date().toISOString() } : { state: "ready", connection: "online", transport: "connected", last_presence_at: new Date().toISOString() },
    agent_health: agentHealth,
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

export type Problem = {
  kind: string;
  source: string;
  phase: string;
  text: string;
  action: [label: string, name: string];
};

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
const NETWORK_ERRORS = new Set(["control_unreachable", "timeout", "dns_failure", "tcp_reset", "tls_failure", "http_5xx", "upgrade_rejected"]);

function makeProblem(kind: string, source: string, phase: string, text: string, action: [string, string]): Problem {
  return { kind, source, phase, text, action };
}

export function agentNeedsAttention(s: Snapshot | null) {
  return s?.agent_health === "offline";
}

const TRANSIENT_FAILURES = new Set([
  "agent_unreachable", "control_unreachable", "timeout", "dns_failure", "tcp_reset", "tls_failure",
  "http_5xx", "upgrade_rejected", "profile_cleanup_pending", "cleanup_pending", "logout_in_progress",
]);

export function transientFailure(code: unknown) {
  const value = typeof code === "string" ? code : code instanceof Error ? code.message : "";
  return TRANSIENT_FAILURES.has(value);
}

function networkWarningDue(status: NonNullable<Status>, now: number) {
  const transportFailure = !!status.transport_error && (NETWORK_ERRORS.has(status.transport_error) || status.transport_error.startsWith("control_closed_"));
  const reference = transportFailure
    ? status.transport_failure_since
    : status.presence_failure_since ?? status.last_presence_at;
  if (!reference) return false;
  const at = Date.parse(reference);
  return Number.isFinite(at) && now - at >= CONNECTION_GRACE_MS;
}

/** Maps only known failures that need a direct user action. */
export function problem(s: Snapshot | null, now = Date.now()): Problem | null {
  const st = s?.status;
  const agentHealth = s?.agent_health ?? (st ? "healthy" : "checking");
  if (agentHealth === "offline") return makeProblem("agent", "local_agent", "health_check", "本机服务暂时无法连接", ["立即重试", "reconnect"]);
  if (agentHealth !== "healthy" || !st) return null;
  const err = st.error || "";
  const source = st.error_source || "";
  const phase = st.error_phase || "";
  if (st.cleanup_state === "isolated" || (source === "session_cleanup" && phase === "isolated") || err === "session_quarantined") return null;
  if (st.authorization === "reauth_required" || AUTH_ERRORS.has(err)) {
    return makeProblem("authorization", "authorization", "authorization", AUTH_MESSAGES[err] ?? "LotusHub 授权失效", ["重新连接", "reconnect"]);
  }
  if (err === "proxy_auth_failed") {
    return makeProblem("proxy", source || "server_connection", phase || "proxy_authentication", "系统代理认证失败", ["立即重连", "reconnect"]);
  }
  if (s.engine_error || /^driver_download_/.test(err) || err === "driver_launcher_checksum_mismatch") {
    return makeProblem("download", source || "local_component", phase || "download", "组件下载失败", ["换个线路重试", "driver_retry"]);
  }
  if (source === "local_component" || s.engine_ready && st.driver_ready === false || err.startsWith("driver_") || err.startsWith("engine_")) {
    return makeProblem("component", source || "local_component", phase || "repair", "浏览器组件需要检查", ["一键修复", "repair"]);
  }
  if (err === "control_protocol_mismatch" || err === "command_trust_not_configured" || err === "command_key_mismatch") {
    return makeProblem("service", source || "server_connection", phase || "service_configuration", "LotusHub 服务配置需要处理", ["重新连接", "reconnect"]);
  }
  if (source === "profile_sync" && phase !== "syncing") {
    return makeProblem("profile_sync", source, phase || "attention", "档案同步需要处理", ["查看日志", "logs"]);
  }
  const transportIssue = !!st.transport_error && (NETWORK_ERRORS.has(st.transport_error) || st.transport_error.startsWith("control_closed_"));
  const businessRefusal = source === "server_connection" && phase === "service_error";
  const connectionIssue = !businessRefusal && (st.connection !== "online" || transportIssue || NETWORK_ERRORS.has(err) || err.startsWith("control_closed_"));
  if (connectionIssue) {
    if (networkWarningDue(st, now)) {
      return makeProblem("network", source || "server_connection", phase || "unavailable", "暂时无法连接 LotusHub", ["立即重连", "reconnect"]);
    }
    return null;
  }
  return null;
}

export function statusLabel(s: Snapshot | null, p: Problem | null = problem(s)): string {
  const status = s?.status;
  const agentHealth = s?.agent_health ?? (status ? "healthy" : "checking");
  if (status?.cleanup_state === "isolated" || (status?.error_source === "session_cleanup" && status.error_phase === "isolated")) return "会话已隔离";
  if (status?.cleanup_state === "closing") return "正在关闭浏览器并清理本机状态";
  if (status?.cleanup_state === "awaiting_server") return "浏览器已关闭，正在同步清理状态";
  if (agentHealth === "checking") return "正在启动本地组件";
  if (agentHealth === "retrying") return "本机服务正在重连";
  if (p) return p.text;
  if (status?.error_source === "profile_sync") {
    return status.error_phase === "syncing" ? "档案正在同步" : "档案同步需要处理";
  }
  if (status?.connection !== "online") return "正在重连";
  return "服务连接正常";
}

export function sessionLabel(status: Status | undefined): string {
  if (!status) return "会话状态未知";
  if (status.cleanup_state === "isolated") return "会话已隔离";
  if (status.cleanup_state === "closing") return "正在清理本机会话";
  if (status.cleanup_state === "awaiting_server") return "等待服务端确认清理";
  if (status.cleanup_state === "complete") return "会话清理已确认";
  if (status.cleanup_state === "active" || status.session_active) return "会话运行中";
  return "无待处理会话";
}

export function presenceTime(value?: string | null) {
  if (!value) return "尚未成功连接";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "尚未成功连接";
  return new Intl.DateTimeFormat("zh-CN", { hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23" }).format(date);
}

export function act(name: string, snap: Snapshot | null) {
  if (name === "repair") return invoke("shell_action", { name: "probe" }).then(() => invoke("shell_action", { name: "retry_cleanup" }));
  if (name === "driver_retry" && snap) {
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
