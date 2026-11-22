import { cn } from "@proxyshard/shardx-ui-kit";
import type { MouseEvent, ReactNode } from "react";
import { invoke } from "./bridge";
import { CloseGlyph, GearGlyph, MaximizeGlyph, MinimizeGlyph, RestoreGlyph } from "./glyphs";
import lotus from "./lotus-16.svg";

const win = (op: string) => () => void invoke("shell_window", { op });

/** Mouse down on empty bar: drag the window; a double click toggles maximize. */
export function onDragArea(e: MouseEvent) {
  if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
  void invoke("shell_window", { op: e.detail === 2 ? "toggle_maximize" : "drag" });
}

type CaptionProps = {
  label: string;
  onClick: () => void;
  children: ReactNode;
  tone?: "default" | "close";
  pressed?: boolean;
};

/// One caption button: 46 x 36 (Windows 11's size), square, flush with the
/// edge. Hover is a tint, never a border; Close turns Windows' red. The name
/// is a native `title`: a kit tooltip would be clipped by this 36 px view.
export function Caption({ label, onClick, children, tone = "default", pressed }: CaptionProps) {
  return (
    <button
      type="button"
      title={label}
      aria-label={label}
      aria-pressed={pressed}
      onClick={onClick}
      className={cn(
        "relative grid h-9 w-[46px] shrink-0 place-items-center text-icon-sub-600 outline-none focus-ring-inset",
        "transition-[background-color,color] duration-(--motion-fast) ease-out reduced:transition-none",
        tone === "close"
          ? "hover:bg-[#c42b1c] hover:text-static-white active:bg-[#b3261a] active:text-static-white"
          : "hover:bg-bg-hover hover:text-icon-strong-950 active:bg-bg-active",
        pressed && "bg-bg-hover text-primary-base hover:text-primary-base",
      )}
    >
      {children}
    </button>
  );
}

export function Brand() {
  return (
    <span className="flex items-center gap-2 pl-3">
      <img src={lotus} width={16} height={16} alt="" draggable={false} />
      <span className="text-ui font-medium tracking-[0.01em] text-text-strong-950">LotusProxy</span>
    </span>
  );
}

export function WindowButtons({ maximized, withMaximize = true }: { maximized?: boolean; withMaximize?: boolean }) {
  return (
    <>
      <Caption label="最小化" onClick={win("minimize")}>
        <MinimizeGlyph />
      </Caption>
      {withMaximize && (
        <Caption label={maximized ? "向下还原" : "最大化"} onClick={win("toggle_maximize")}>
          {maximized ? <RestoreGlyph /> : <MaximizeGlyph />}
        </Caption>
      )}
      <Caption label="关闭到托盘" tone="close" onClick={win("close")}>
        <CloseGlyph />
      </Caption>
    </>
  );
}

/** The daily screen's whole main view: brand, settings, the window buttons. */
export function TitleBar({ drawer, attention, maximized }: { drawer: boolean; attention: boolean; maximized?: boolean }) {
  return (
    <header className="flex h-9 items-center border-b border-stroke-soft-200 bg-bg-white-0" onMouseDown={onDragArea}>
      <div className="flex h-full flex-1 items-center">
        <Brand />
      </div>
      <Caption label={drawer ? "收起设置" : "设置"} pressed={drawer} onClick={() => void invoke("shell_set_drawer", { open: !drawer })}>
        <GearGlyph />
        {attention && (
          <span className="absolute right-[14px] top-[9px] size-[6px] rounded-full bg-warning-base ring-2 ring-bg-white-0" aria-hidden="true" />
        )}
      </Caption>
      <span className="mx-1 h-3.5 w-px bg-stroke-soft-200" aria-hidden="true" />
      <WindowButtons maximized={maximized} />
    </header>
  );
}
