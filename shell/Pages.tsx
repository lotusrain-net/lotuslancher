import { Button, ProgressBar } from "@proxyshard/shardx-ui-kit";
import { useEffect, useState, type ReactNode } from "react";
import { act, invoke, useEvent, type Progress, type Snapshot } from "./bridge";
import { Settings } from "./Settings";
import { Brand, onDragArea, WindowButtons } from "./TitleBar";

type Action = { label: string; run: () => void; secondary?: boolean };
type Spec = { spinner?: boolean; title: string; text: string; progress?: boolean; actions?: Action[] };

function spec(s: Snapshot, openSettings: () => void, downloadProgress: Progress): Spec {
  const settings: Action = { label: "打开设置", run: openSettings, secondary: true };
  if (s.agent_health === "offline") {
    return {
      spinner: true,
      title: "本机服务暂时无法连接",
      text: "客户端正在重试本机服务连接。",
      actions: [{ label: "立即重试", run: () => void invoke("shell_reconnect") }, settings],
    };
  }
  switch (s.screen) {
    case "init": {
      const failed = !!s.engine_error;
      const installing = s.engine_installing;
      const oldEngine = s.engine_installed_version?.replace(/^(Chrome|Chromium)\//, "").split(".")[0];
      const newEngine = s.engine_required_version?.replace(/^(Chrome|Chromium)\//, "").split(".")[0];
      const updating = !!oldEngine && !!newEngine && oldEngine !== newEngine;
      const downloadStatus = downloadProgress?.route
        ? `正在通过${downloadProgress.route === "system_proxy" ? "系统代理" : "直连网络"}下载${updating ? `浏览器引擎更新（${oldEngine} → ${newEngine}）` : "浏览器组件"}（第 ${downloadProgress.attempt ?? 1}/${downloadProgress.max_attempts ?? 5} 次）…`
        : "下载完成后会自动进入，不需要任何操作。";
      return {
        spinner: installing,
        title: updating ? "正在更新浏览器引擎" : "首次使用，正在准备浏览器组件",
        text: failed ? "组件下载失败，请检查网络后重试。" : installing ? downloadStatus : "组件已就绪，正在进入…",
        progress: installing,
        actions: failed ? [{ label: "换个线路重试", run: () => void act("driver_retry", s) }, settings] : [settings],
      };
    }
    case "offline":
      return {
        title: "无法连接 LotusHub",
        text: "请检查网络连接，或在设置中查看诊断信息。",
        actions: [{ label: "重新连接", run: () => void invoke("shell_reconnect") }, settings],
      };
    case "quitting":
      return { spinner: true, title: "正在安全清理临时浏览器数据…", text: "完成后客户端会自动退出，请勿强制关闭。" };
    default:
      if (s.agent_health === "healthy") {
        return { spinner: true, title: "正在连接 LotusHub", text: "本机服务已就绪，正在加载工作区…" };
      }
      return {
        spinner: true,
        title: "正在启动本地组件",
        text: s.agent_health === "retrying" ? "本机服务暂时未响应，正在重试。" : "正在等待本地 Agent 健康检查…",
      };
  }
}

const Spinner = () => (
  <span className="mx-auto block size-7 animate-spin rounded-full border-[2.5px] border-stroke-soft-200 border-t-primary-base reduced:animate-none" aria-hidden="true" />
);

/// Keeps a panel mounted for its exit transition, then unmounts it.
export function usePresence(open: boolean, exitMs: number) {
  const [mounted, setMounted] = useState(open);
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (open) {
      setMounted(true);
      // Two frames: the first paints the closed state, the second starts the transition.
      const a = requestAnimationFrame(() => requestAnimationFrame(() => setShown(true)));
      return () => cancelAnimationFrame(a);
    }
    setShown(false);
    const t = setTimeout(() => setMounted(false), exitMs);
    return () => clearTimeout(t);
  }, [open, exitMs]);
  return { mounted, shown };
}

/// The first-run, loading, offline and quitting screens: the whole window,
/// with their own settings panel sliding in from the right.
export function Page({ snap, progress, refresh }: { snap: Snapshot; progress: Progress; refresh: () => void }) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const canSettings = snap.screen !== "quitting";
  const open = settingsOpen && canSettings;
  const { mounted, shown } = usePresence(open, 160);
  const sp = spec(snap, () => setSettingsOpen(true), progress);

  useEvent("shell://open-settings", () => setSettingsOpen(true));
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setSettingsOpen(false);
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const percent = Math.max(2, Math.min(100, progress?.percent ?? 0));
  const body: ReactNode = (
    <div className="shell-page-card max-w-[440px] text-center" key={snap.screen}>
      {sp.spinner && <Spinner />}
      <h1 className="mb-2 mt-4 text-page-title text-text-strong-950">{sp.title}</h1>
      <p className="mb-5 text-body text-text-sub-600">{sp.text}</p>
      {sp.progress && progress && (
        <div className="mx-auto mb-5 w-64">
          <ProgressBar value={percent} max={100} aria-label="下载进度" />
        </div>
      )}
      {sp.actions && (
        <div className="flex flex-wrap justify-center gap-2.5">
          {sp.actions.map((a) => (
            <Button key={a.label} size="small" mode={a.secondary ? "stroke" : "filled"} variant={a.secondary ? "neutral" : "primary"} onClick={a.run}>
              {a.label}
            </Button>
          ))}
        </div>
      )}
    </div>
  );

  return (
    <main className="fixed inset-0 flex flex-col bg-bg-page">
      <div className="flex h-9 items-center" onMouseDown={onDragArea}>
        <div className="flex h-full flex-1 items-center">
          <Brand />
        </div>
        <WindowButtons withMaximize={false} />
      </div>
      <section className="flex flex-1 items-center justify-center p-6">{body}</section>
      {mounted && (
        <>
          <div
            className="shell-scrim fixed inset-x-0 bottom-0 top-9 bg-[#0e121b]/10"
            data-shown={shown || undefined}
            onClick={() => setSettingsOpen(false)}
            aria-hidden="true"
          />
          <aside className="shell-sheet fixed bottom-0 right-0 top-9 w-[360px] overflow-y-auto border-l border-stroke-soft-200 bg-bg-page shadow-[var(--shadow-lg)]" data-shown={shown || undefined}>
            <Settings snap={snap} onClose={() => setSettingsOpen(false)} refresh={refresh} isOpen={settingsOpen} />
          </aside>
        </>
      )}
    </main>
  );
}

