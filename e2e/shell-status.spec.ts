import { expect, test, type Page } from "@playwright/test";
import { appModule, openApp, NOW } from "./support/app";

type Bridge = typeof import("../shell/bridge");
type Win = { __shellBridge: Bridge };

async function loadBridge(page: Page) {
  await openApp(page);
  await appModule(page, "/shell/bridge.ts", "__shellBridge");
}

test("startup waits for the local Agent health response", async ({ page }) => {
  await page.goto("/shell/index.html?screen=starting");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("正在启动本地组件");
  await expect(page.getByText("正在等待本地 Agent 健康检查…")).toBeVisible();

  await page.evaluate(() => {
    const internals = (window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (cmd: string, args?: Record<string, unknown>, options?: unknown) => Promise<unknown>;
      };
    }).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (cmd, args, options) => cmd === "shell_snapshot"
      ? Promise.resolve({
        screen: "starting", drawer: false, status: { connection: "reconnecting" },
        agent_health: "healthy", engine_ready: true, engine_installing: false,
        engine_error: null, shell_version: "1.2.11", autostart_available: true,
      })
      : invoke(cmd, args, options);
  });
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("正在连接 LotusHub");
  await expect(page.getByText("本机服务已就绪，正在加载工作区…")).toBeVisible();
});

test("unknown business errors do not become component failures", async ({ page }) => {
  await loadBridge(page);
  const result = await page.evaluate((now) => {
    const bridge = (window as unknown as Win).__shellBridge;
    const snap = {
      screen: "daily" as const, drawer: false, status: {
        connection: "online", error: "business_refused", error_source: "operation", error_phase: "action",
      }, agent_health: "healthy" as const, engine_ready: true, engine_installing: false,
      engine_error: null, shell_version: "1.2.11", autostart_available: true,
    };
    return { problem: bridge.problem(snap, now), label: bridge.statusLabel(snap) };
  }, NOW.getTime());
  expect(result).toEqual({ problem: null, label: "服务连接正常" });
});

test("server reconnect stays quiet during the grace period, then offers retry", async ({ page }) => {
  await loadBridge(page);
  const result = await page.evaluate((now) => {
    const bridge = (window as unknown as Win).__shellBridge;
    const base = {
      screen: "daily" as const, drawer: false, status: {
        connection: "reconnecting", transport: "disconnected",
        presence_failure_since: new Date(now - 89_000).toISOString(),
        last_presence_at: new Date(now - 90_000).toISOString(),
      }, agent_health: "healthy" as const, engine_ready: true, engine_installing: false,
      engine_error: null, shell_version: "1.2.11", autostart_available: true,
    };
    const quiet = bridge.problem(base, now);
    base.status.presence_failure_since = new Date(now - 90_000).toISOString();
    const due = bridge.problem(base, now);
    return { quiet, due, label: bridge.statusLabel(base, due) };
  }, NOW.getTime());
  expect(result.quiet).toBeNull();
  expect(result.due).toMatchObject({ kind: "network", source: "server_connection", phase: "unavailable", action: ["立即重连", "reconnect"] });
  expect(result.label).toBe("暂时无法连接 LotusHub");
});

test("the gear indicator follows local IPC health only", async ({ page }) => {
  await loadBridge(page);
  const result = await page.evaluate(() => {
    const bridge = (window as unknown as Win).__shellBridge;
    const base = {
      screen: "daily" as const, drawer: false, status: { connection: "reconnecting" },
      agent_health: "healthy" as const, engine_ready: true, engine_installing: false,
      engine_error: null, shell_version: "1.2.11", autostart_available: true,
    };
    return {
      remoteDown: bridge.agentNeedsAttention(base),
      checking: bridge.problem({ ...base, status: null, agent_health: "checking" }),
      localDown: bridge.agentNeedsAttention({ ...base, agent_health: "offline" }),
    };
  });
  expect(result.remoteDown).toBe(false);
  expect(result.checking).toBeNull();
  expect(result.localDown).toBe(true);
});

test("the last confirmed session state stays visible during server reconnect", async ({ page }) => {
  await loadBridge(page);
  const result = await page.evaluate(() => {
    const bridge = (window as unknown as Win).__shellBridge;
    const status = { connection: "reconnecting", session_active: true, cleanup_state: "active" as const };
    const snap = {
      screen: "daily" as const, drawer: false, status, agent_health: "healthy" as const,
      engine_ready: true, engine_installing: false, engine_error: null,
      shell_version: "1.2.11", autostart_available: true,
    };
    return { connection: bridge.statusLabel(snap), session: bridge.sessionLabel(status) };
  });
  expect(result).toEqual({ connection: "正在重连", session: "会话运行中" });
});

test("cleanup confirmation, isolation and profile sync have distinct status text", async ({ page }) => {
  await loadBridge(page);
  const labels = await page.evaluate(() => {
    const bridge = (window as unknown as Win).__shellBridge;
    const base = {
      screen: "daily" as const, drawer: false, agent_health: "healthy" as const,
      engine_ready: true, engine_installing: false, engine_error: null,
      shell_version: "1.2.11", autostart_available: true,
    };
    const label = (status: NonNullable<Bridge["Status"]>) => bridge.statusLabel({ ...base, status });
    return [
      label({ connection: "reconnecting", cleanup_state: "awaiting_server", session_active: false }),
      label({ connection: "reconnecting", cleanup_state: "isolated", session_active: false }),
      label({ connection: "online", error_source: "profile_sync", error_phase: "syncing" }),
    ];
  });
  expect(labels).toEqual([
    "浏览器已关闭，正在同步清理状态",
    "会话已隔离",
    "档案正在同步",
  ]);
});
