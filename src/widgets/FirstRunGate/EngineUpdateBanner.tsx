import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Button, CopyButton, ProgressBar, cn } from "@proxyshard/shardx-ui-kit";
import { AlertCircleIcon } from "../../shared/icons";
import { useT } from "../../shared/i18n";
import { clip } from "../../shared/lib/clipboard";
import { fmtBytesProgress } from "../../shared/lib/format";
import { useRunningCount } from "../../entities/profile";
import { HAS_CAPTIONS } from "../WindowControls";
import type { RtInstallError, RtProgress } from "../../shared/types";
import { joined, reason, retryEngineUpdate, stopAndFinish, useEngineUpdate } from "./engineUpdate";

type ShellEngineUpdate = {
  engine_update: "checking" | "current" | "downloading" | "updated" | "busy_waiting" | "blocked_launcher" | "failed_network" | "failed_manual";
  engine_installed_version: string | null;
  engine_required_version: string | null;
  min_launcher_version: string | null;
  engine_update_error: RtInstallError | null;
  engine_installing: boolean;
};

const emptyShellUpdate: ShellEngineUpdate = {
  engine_update: "current",
  engine_installed_version: null,
  engine_required_version: null,
  min_launcher_version: null,
  engine_update_error: null,
  engine_installing: false,
};

function useShellEngineUpdate() {
  const [status, setStatus] = useState(emptyShellUpdate);
  const [progress, setProgress] = useState<RtProgress | null>(null);
  const refresh = useCallback(() => {
    void invoke<ShellEngineUpdate>("desktop_engine_update_status").then(setStatus, () => {});
  }, []);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    refresh();
    const timer = window.setInterval(refresh, 2000);
    let off: (() => void) | undefined;
    let gone = false;
    void listen<RtProgress>("runtime:progress", (event) => setProgress(event.payload))
      .then((unlisten) => gone ? unlisten() : (off = unlisten));
    return () => {
      gone = true;
      window.clearInterval(timer);
      off?.();
    };
  }, [refresh]);
  return { status, progress, refresh };
}

function majorVersion(version: string | null) {
  return version?.replace(/^(Chrome|Chromium)\//, "").split(".")[0] ?? "";
}

function shellUpdateCopy(status: ShellEngineUpdate, progress: RtProgress | null, t: ReturnType<typeof useT>) {
  const installed = majorVersion(status.engine_installed_version);
  const required = majorVersion(status.engine_required_version);
  switch (status.engine_update) {
    case "downloading":
      return {
        title: "正在更新浏览器引擎",
        detail: progress?.phase === "download"
          ? `正在下载更新（${fmtBytesProgress(progress.received, progress.total)}）`
          : progress ? t("firstRunGate.extracting") : t("firstRunGate.contactingCdn"),
      };
    case "updated":
      return { title: "浏览器引擎已更新", detail: `已更新到 ${required || installed}，新会话将使用它。` };
    case "busy_waiting":
      return { title: "浏览器引擎更新稍后重试", detail: "当前会话结束后会自动重试一次。" };
    case "blocked_launcher":
      return { title: "需要先更新壳", detail: `请先更新壳到 ${status.min_launcher_version || "新版本"}。` };
    case "failed_network":
      return { title: "无法检查引擎更新", detail: "请检查网络连接后重试。" };
    case "failed_manual": {
      const error = status.engine_update_error;
      const detail = error ? reason(t, error) : "请检查更新原因后手动重试。";
      return { title: "浏览器引擎更新未完成", detail };
    }
    default:
      return null;
  }
}

/// An engine update that stopped with the previous engine kept (#122): one
/// slim tile over the page, as the team's status is, with Try again. Stopped
/// by running profiles, it goes in once the last one closes.
export function EngineUpdateBanner() {
  const t = useT();
  const shellUpdate = useShellEngineUpdate();
  const [shellRetrying, setShellRetrying] = useState(false);
  const error = useEngineUpdate((s) => s.error);
  const staged = useEngineUpdate((s) => s.staged);
  const retrying = useEngineUpdate((s) => s.retrying);
  const progress = useEngineUpdate((s) => s.progress);
  const shellCopy = shellUpdateCopy(shellUpdate.status, shellUpdate.progress, t);
  const running = useRunningCount();
  const before = useRef(running);
  useEffect(() => {
    const was = before.current;
    before.current = running;
    if (!(was > 0 && running === 0)) return;
    // A browser may still be exiting when its profile shows stopped: a few tries, further apart.
    let gone = false;
    void (async () => {
      for (const ms of [800, 2500, 6000]) {
        await new Promise((r) => setTimeout(r, ms));
        if (gone || useEngineUpdate.getState().error?.stage !== "busy") return;
        await retryEngineUpdate(true);
      }
    })();
    return () => { gone = true; };
  }, [running]);
  if (shellCopy) {
    const status = shellUpdate.status;
    const error = status.engine_update_error;
    const retry = async () => {
      if (shellRetrying) return;
      setShellRetrying(true);
      try {
        await invoke("shell_install_engine");
        shellUpdate.refresh();
      } finally {
        setShellRetrying(false);
      }
    };
    return (
      <div
        data-engine-update={status.engine_update}
        className={cn("mx-gutter mt-2.5 flex shrink-0 items-center gap-2 rounded-control bg-warning-tint px-3 py-1 text-secondary text-text-strong-950 forced-edge", HAS_CAPTIONS && "me-[150px]")}
      >
        <div className="min-w-0 flex-1 py-1">
          <p role="status" className="m-0 min-w-0 [overflow-wrap:anywhere]">
            <strong className="font-medium">{shellCopy.title}</strong> {shellCopy.detail}
          </p>
          {status.engine_update === "downloading" && shellUpdate.progress && (
            <ProgressBar aria-label="引擎更新进度" value={shellUpdate.progress.percent} color="primary" />
          )}
        </div>
        {error?.message && <CopyButton size="2xsmall" className="shrink-0" label={t("kit.copyDetails")} onCopy={async () => { await clip.write(error.message); }} />}
        {status.engine_update !== "downloading" && (
          <Button variant="neutral" mode="stroke" size="2xsmall" className="shrink-0" isLoading={shellRetrying} onClick={() => void retry()}>
            {t("firstRunGate.retry")}
          </Button>
        )}
      </div>
    );
  }
  if (!error) return null;
  const title = retrying ? t("engineUpdate.updatingTitle") : t("engineUpdate.title");
  // The fact first, then what to do.
  const detail = !retrying
    ? [staged ? t("engineUpdate.staged") : "", reason(t, error), t("engineUpdate.kept")].filter(Boolean).reduce(joined)
    : staged ? t("engineUpdate.finishing")
      : progress?.phase === "download" ? t("engineUpdate.downloaded", { progress: fmtBytesProgress(progress.received, progress.total) })
        : progress ? t("firstRunGate.extracting") : t("firstRunGate.contactingCdn");
  // Try again would meet the same profiles: stopping them is the way on.
  const stopFirst = error.stage === "busy" && running > 0 && !retrying;
  return (
    <div
      data-engine-update={error.stage}
      // On Windows and Linux the caption buttons sit over this row (#142): their room, as PageHeader keeps.
      className={cn("mx-gutter mt-2.5 flex shrink-0 items-center gap-2 rounded-control bg-warning-tint px-3 py-1 text-secondary text-text-strong-950 forced-edge", HAS_CAPTIONS && "me-[150px]")}
    >
      <div className="flex min-w-0 flex-1 items-start gap-2 py-1">
        <AlertCircleIcon aria-hidden="true" className="mt-0.5 size-3.5 shrink-0 text-warning-text" />
        <p role="status" className="m-0 min-w-0 [overflow-wrap:anywhere]">
          <strong className="font-medium">{title}</strong>
          {/[。！？]$/.test(title) ? "" : " "}
          <span className="text-text-sub-600">{detail}</span>
        </p>
      </div>
      {/* The backend's own words, with the paths and numbers, for a report. */}
      {error.message && <CopyButton size="2xsmall" className="shrink-0" label={t("kit.copyDetails")} onCopy={async () => { await clip.write(error.message); }} />}
      {stopFirst ? (
        <Button variant="neutral" mode="stroke" size="2xsmall" className="shrink-0" onClick={() => void stopAndFinish()}>
          {running === 1 ? t("extensions.stopAndUpdateOne", { n: running }) : t("extensions.stopAndUpdateMany", { n: running })}
        </Button>
      ) : (
        <Button variant="neutral" mode="stroke" size="2xsmall" className="shrink-0" isLoading={retrying} onClick={() => void retryEngineUpdate()}>
          {t("firstRunGate.retry")}
        </Button>
      )}
    </div>
  );
}
