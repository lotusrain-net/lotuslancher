import { Button, ProgressBar } from "@proxyshard/shardx-ui-kit";
import { useState } from "react";
import { invoke, useEvent, type Progress, type Snapshot } from "./bridge";

function version(value: string | null) {
  return value?.replace(/^(Chrome|Chromium)\//, "") ?? "";
}

function text(snap: Snapshot) {
  const installed = version(snap.engine_installed_version);
  const required = version(snap.engine_required_version);
  switch (snap.engine_update) {
    case "downloading":
      return ["正在更新浏览器引擎", installed && required ? `${installed} → ${required}` : "正在下载并安装更新"] as const;
    case "updated":
      return ["浏览器引擎已更新", `已更新到 ${required || installed}，新会话将使用它。`] as const;
    case "busy_waiting":
      return ["浏览器引擎更新稍后重试", "当前会话结束后会自动重试一次。"] as const;
    case "blocked_launcher":
      return ["需要更新 LotusProxy", `请先更新壳到 ${snap.min_launcher_version || "新版本"}。`] as const;
    case "failed_network":
      return ["无法检查引擎更新", "请检查网络连接后重试。"] as const;
    case "failed_manual":
      return ["浏览器引擎更新未完成", manualFailureDetail(snap)] as const;
    default:
      return null;
  }
}

function manualFailureDetail(snap: Snapshot) {
  switch (snap.engine_update_error?.stage) {
    case "space": return "磁盘空间不足，请清理空间后手动重试。";
    case "verify": return "引擎文件校验失败，请检查磁盘状态后手动重试。";
    case "extract": return "引擎解包失败，请检查磁盘状态后重试。";
    case "swap": return "引擎切换失败，请重试。";
    default: return snap.engine_update_error?.message || "请检查更新原因后手动重试。";
  }
}

export function EngineUpdateBanner({ snap }: { snap: Snapshot }) {
  const [progress, setProgress] = useState<Progress>(null);
  const [retrying, setRetrying] = useState(false);
  useEvent<Progress>("runtime:progress", setProgress);
  useEvent<null>("runtime:done", () => setProgress(null));
  const copy = text(snap);
  if (!copy) return null;

  const retry = async () => {
    if (retrying) return;
    setRetrying(true);
    try {
      await invoke("shell_install_engine");
    } finally {
      setRetrying(false);
    }
  };
  const percent = Math.max(2, Math.min(100, progress?.percent ?? 0));

  return (
    <section
      data-engine-update={snap.engine_update}
      className="flex h-[52px] shrink-0 items-center gap-3 border-b border-stroke-soft-200 bg-warning-tint px-4 text-secondary text-text-strong-950"
      role="status"
    >
      <strong className="shrink-0 font-medium">{copy[0]}</strong>
      <span className="min-w-0 flex-1 truncate text-text-sub-600">{copy[1]}</span>
      {snap.engine_update === "downloading" && progress && <ProgressBar value={percent} max={100} aria-label="引擎更新进度" />}
      {snap.engine_update !== "downloading" && (
        <Button size="2xsmall" mode="stroke" variant="neutral" isLoading={retrying} onClick={() => void retry()}>
          重试
        </Button>
      )}
    </section>
  );
}
