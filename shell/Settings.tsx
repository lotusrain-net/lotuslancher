import { AlertModal, Button, Card, Input, Switch, Toast, cn } from "@proxyshard/shardx-ui-kit";
import { useCallback, useEffect, useId, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { act, invoke, presenceTime, problem, say, sessionLabel, statusLabel, transientFailure, type Prefs, type Snapshot } from "./bridge";
import { CloseGlyph } from "./glyphs";

type Note = { status: "success" | "error"; text: string } | null;
const DEFAULT_CONTROL_URL = "https://hub.lotusrain.net";

function validControlOrigin(raw: string): boolean {
  if (raw.trim() === "") return true;
  try {
    const url = new URL(raw.trim());
    const loopback = url.hostname === "localhost" || url.hostname === "127.0.0.1" || url.hostname === "::1" || url.hostname === "[::1]";
    return (url.protocol === "https:" || url.protocol === "http:" && loopback)
      && url.username === "" && url.password === "" && url.pathname === "/"
      && url.search === "" && url.hash === "";
  } catch {
    return false;
  }
}

function normalizedControlValue(raw: string | null | undefined): string {
  const value = raw?.trim() || DEFAULT_CONTROL_URL;
  try {
    return new URL(value).origin;
  } catch {
    return value;
  }
}

function copy(text: string) {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(text);
  const ta = document.createElement("textarea");
  ta.value = text;
  document.body.appendChild(ta);
  ta.select();
  document.execCommand("copy");
  ta.remove();
  return Promise.resolve();
}

/** One settings block; `--i` staggers its entrance (shell.css). */
function Section({ title, index, children }: { title: string; index: number; children: ReactNode }) {
  return (
    <Card variant="surface" padding="md" className="shell-enter" style={{ "--i": index } as CSSProperties}>
      <h3 className="mb-3 text-section-label text-text-sub-600">{title}</h3>
      {children}
    </Card>
  );
}

function Row({ label, hint, children }: { label: string; hint?: string; children: (labelId: string) => ReactNode }) {
  const id = useId();
  return (
    <div className="flex items-center justify-between gap-4 border-t border-stroke-soft-200 py-2.5 first:border-t-0 first:pt-0 last:pb-0">
      <div className="min-w-0">
        <div id={id} className="text-body text-text-strong-950">{label}</div>
        {hint && <div className="mt-0.5 text-help text-text-soft-400">{hint}</div>}
      </div>
      {children(id)}
    </div>
  );
}

/// The settings: status and repair, two preferences, logs, temporary data,
/// version and uninstall. Drawn in the drawer view and beside the
/// full-window pages; `onClose` closes whichever holds it.
export function Settings({ snap, onClose, refresh, isOpen }: { snap: Snapshot | null; onClose: () => void; refresh: () => void; isOpen: boolean }) {
  const [prefs, setPrefs] = useState<Prefs | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [prefsError, setPrefsError] = useState(false);
  const [note, setNote] = useState<Note>(null);
  const [confirmUninstall, setConfirmUninstall] = useState(false);
  const [controlUrl, setControlUrl] = useState(DEFAULT_CONTROL_URL);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const prefsLoad = useRef<Promise<void> | null>(null);
  const syncDraftAfterPrefsLoad = useRef(false);

  const loadPrefs = useCallback((options: { syncControlUrl?: boolean } = {}) => {
    if (options.syncControlUrl) syncDraftAfterPrefsLoad.current = true;
    if (prefsLoad.current) return prefsLoad.current;
    const pending = invoke<Prefs>("shell_preferences").then((next) => {
      setPrefs(next);
      setPrefsError(false);
      if (syncDraftAfterPrefsLoad.current) {
        setControlUrl(next.control_url?.trim() || DEFAULT_CONTROL_URL);
        syncDraftAfterPrefsLoad.current = false;
      }
    }, () => {
      setPrefsError(true);
      syncDraftAfterPrefsLoad.current = false;
    }).finally(() => { prefsLoad.current = null; });
    prefsLoad.current = pending;
    return pending;
  }, []);
  useEffect(() => {
    if (isOpen) void loadPrefs({ syncControlUrl: true });
  }, [isOpen, loadPrefs]);

  const toast = (n: Note) => {
    setNote(n);
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setNote(null), 3000);
  };

  const run = (key: string, p: () => Promise<unknown>, ok?: string) => {
    setBusy(key);
    p().then(
      () => ok && toast({ status: "success", text: ok }),
      (e) => { if (!transientFailure(e)) toast({ status: "error", text: say(e) }); },
    ).finally(() => {
      setBusy(null);
      refresh();
    });
  };

  const p = problem(snap);
  const active = !!snap?.status?.session_active;
  const status = snap?.status;
  const agentHealth = snap?.agent_health ?? (status ? "healthy" : "checking");
  const connecting = agentHealth !== "healthy" || !status || status.connection !== "online";
  const connectedRecently = !!status?.last_presence_at;
  const statusText = statusLabel(snap, p);
  const transportText = status?.transport === "connected"
    ? "实时通道已连接"
    : status?.connection !== "online" || connectedRecently
      ? "实时通道正在重连"
      : "实时通道尚未连接";
  const controlUrlDirty = normalizedControlValue(controlUrl) !== normalizedControlValue(prefs?.control_url);
  const badControlUrl = !validControlOrigin(controlUrl);
  const canEditControlUrl = !!prefs?.control_url_editable && !prefs.control_url_overridden;

  return (
    <div className="flex min-h-full flex-col">
      <div className="shell-enter flex items-center justify-between px-4 pb-2 pt-3.5" style={{ "--i": 0 } as CSSProperties}>
        <h2 className="text-panel-title text-text-strong-950">设置</h2>
        <button
          type="button"
          aria-label="关闭设置"
          title="关闭设置"
          onClick={onClose}
          className="grid size-7 place-items-center rounded-control text-icon-sub-600 outline-none transition-colors duration-(--motion-fast) hover:bg-bg-hover hover:text-icon-strong-950 focus-ring"
        >
          <CloseGlyph />
        </button>
      </div>

      {prefsError && (
        <div className="mx-4 mb-2 flex items-center justify-between gap-3 rounded-control bg-bg-weak-50 px-3 py-2 text-help text-error-text" role="alert">
          <span>桌面端偏好读取失败，保留当前设置；可重试加载。</span>
          <Button size="2xsmall" mode="lighter" onClick={() => void loadPrefs({ syncControlUrl: true })}>重试</Button>
        </div>
      )}

      <div className="flex flex-col gap-3 px-4 pb-6 pt-2">
        <Section title="状态与诊断" index={1}>
          <div className="mb-3 flex items-center gap-2">
            <span className={cn("size-2 rounded-full", p || connecting ? "bg-warning-base" : "bg-success-base")} aria-hidden="true" />
            <span className="text-body font-medium text-text-strong-950">{statusText}</span>
            {p && (
              <Button
                size="2xsmall"
                mode="lighter"
                className="ml-auto"
                isLoading={busy === "fix"}
                onClick={() => run("fix", () => act(p.action[1], snap), "已重试")}
              >
                {p.action[0]}
              </Button>
            )}
          </div>
          <details className="border-t border-stroke-soft-200 pt-2.5">
            <summary className="flex cursor-pointer list-none items-center justify-between rounded-control text-body text-text-sub-600 outline-none focus-ring">
              <span>连接详情</span>
              <span aria-hidden="true" className="text-icon-sub-600">⌄</span>
            </summary>
            <div className="mt-2.5">
              <Row label="最近成功连接">
                {() => <time className="text-body tabular-nums text-text-sub-600">{presenceTime(status?.last_presence_at)}</time>}
              </Row>
              <Row label="通道状态">
                {() => <span className="text-body text-text-sub-600">{transportText}</span>}
              </Row>
              <Row label="会话状态">
                {() => <span className="text-body text-text-sub-600">{sessionLabel(status)}</span>}
              </Row>
            </div>
          </details>
        </Section>

        {(prefs?.control_url_editable || prefs?.control_url_overridden) && <Section title="服务端连接" index={2}>
          <div className="flex flex-col gap-2">
            <label htmlFor="shell-control-url" className="text-body text-text-strong-950">服务端地址</label>
            <Input
              id="shell-control-url"
              aria-label="服务端地址"
              inputSize="xsmall"
              className="mono"
              spellCheck={false}
              disabled={!prefs || !canEditControlUrl || !prefs.settings_writable || busy === "control-url"}
              value={controlUrl}
              placeholder={DEFAULT_CONTROL_URL}
              error={badControlUrl ? "请输入 HTTPS 地址，或本机回环 HTTP 地址。" : undefined}
              onChange={(event) => setControlUrl(event.target.value)}
            />
            <p className="m-0 text-help text-text-soft-400">
              {prefs?.control_url_overridden
                ? "服务端地址由构建配置固定，无法在此修改。"
                : <>远程服务使用 HTTPS；本机 Docker 测试可用 <code>http://127.0.0.1:端口</code>。切换服务端需要彻底退出客户端（包括托盘）并重新打开；切换到不同服务端前，请先退出登录或重置此前绑定的设备状态。</>}
            </p>
            {canEditControlUrl && !prefs?.settings_writable && prefs && (
              <p className="m-0 text-help text-error-text">桌面端设置文件无法读取，当前不能修改服务端地址。</p>
            )}
            <Button
              size="xsmall"
              mode="stroke"
              variant="neutral"
              contentAlign="start"
              disabled={!canEditControlUrl || !prefs?.settings_writable || !controlUrlDirty || badControlUrl || busy !== null}
              isLoading={busy === "control-url"}
              onClick={() => run(
                "control-url",
                () => invoke("shell_set_control_url", { url: controlUrl.trim() || null }).then(() => loadPrefs({ syncControlUrl: true })),
                "服务端地址已保存，重启客户端后生效",
              )}
            >
              保存服务端地址
            </Button>
          </div>
        </Section>}

        <Section title="偏好设置" index={3}>
          <Row label="开机自动启动" hint={prefs && !prefs.autostart_available ? "开机自动启动仅在 Windows 上可用" : undefined}>
            {(id) => (
            <Switch
              aria-labelledby={id}
              checked={!!prefs?.autostart}
              disabled={!prefs?.autostart_available}
              onChange={(on) => run("autostart", () => invoke("shell_set_autostart", { on }).then(() => loadPrefs({ syncControlUrl: false })))}
            />
            )}
          </Row>
          <Row label="连接 LotusHub 时使用系统代理">
            {(id) => (
            <Switch
              aria-labelledby={id}
              checked={!!prefs?.use_system_proxy_for_control}
              disabled={!prefs}
              onChange={(on) => run("proxy", () => invoke("shell_set_system_proxy", { on }).then(() => loadPrefs({ syncControlUrl: false })))}
            />
            )}
          </Row>
        </Section>

        <Section title="日志" index={4}>
          <div className="flex flex-col gap-2">
            <Button size="xsmall" mode="stroke" variant="neutral" contentAlign="start" isLoading={busy === "collect"} onClick={() => run("collect", () => invoke("shell_action", { name: "collect_logs" }), "诊断日志包已导出")}>
              导出诊断日志包
            </Button>
            <Button size="xsmall" mode="stroke" variant="neutral" contentAlign="start" onClick={() => run("logs", () => invoke("shell_action", { name: "logs" }))}>
              打开日志文件夹
            </Button>
            <Button size="xsmall" mode="stroke" variant="neutral" contentAlign="start" onClick={() => run("copy", () => invoke<string>("shell_summary").then(copy), "已复制")}>
              复制版本号与状态摘要
            </Button>
          </div>
        </Section>

        <Section title="临时数据" index={5}>
          <Button size="xsmall" mode="stroke" variant="neutral" contentAlign="start" className="w-full" disabled={active} isLoading={busy === "cleanup"} onClick={() => run("cleanup", () => invoke("shell_cleanup"), "临时数据已清理")}>
            清理临时数据
          </Button>
          {active && <p className="mt-2 text-help text-text-soft-400">请先在左侧结束当前会话后再清理</p>}
        </Section>

        <Section title="关于" index={6}>
          <div className="mb-3 flex items-baseline justify-between">
            <span className="text-body text-text-sub-600">版本</span>
            <span className="text-body tabular-nums text-text-strong-950">{snap?.shell_version ?? "-"}</span>
          </div>
          <Button size="xsmall" mode="ghost" variant="error" contentAlign="start" className="-mx-1.5 w-[calc(100%+12px)]" onClick={() => setConfirmUninstall(true)}>
            卸载客户端…
          </Button>
        </Section>
      </div>

      {note && (
        <div className="shell-toast fixed inset-x-4 bottom-4 z-(--layer-toast)">
          <Toast status={note.status} title={note.text} onDismiss={() => setNote(null)} dismissLabel="关闭" />
        </div>
      )}

      <AlertModal
        open={confirmUninstall}
        onOpenChange={setConfirmUninstall}
        status="error"
        title="卸载 LotusProxy 客户端？"
        description="卸载程序会先清理本机数据。"
        cancelLabel="取消"
        confirmLabel="卸载"
        confirmVariant="error"
        onConfirm={() => {
          setConfirmUninstall(false);
          run("remove", () => invoke("shell_action", { name: "remove" }));
        }}
      />
    </div>
  );
}
