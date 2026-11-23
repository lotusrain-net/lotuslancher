// The shell's two local views on one bundle: `#drawer` is the settings
// drawer (its own 360 px webview on the right), anything else the main view
// (the 36 px title bar, or a full-window page).
import { LabelsProvider, ThemeProvider } from "@proxyshard/shardx-ui-kit";
import { StrictMode, useEffect } from "react";
import { createRoot } from "react-dom/client";
import "./shell.css";
import { agentNeedsAttention, invoke, useSnapshot } from "./bridge";
import { Page } from "./Pages";
import { Settings } from "./Settings";
import { TitleBar } from "./TitleBar";

const IS_DRAWER = location.hash === "#drawer";
const closeDrawer = () => void invoke("shell_set_drawer", { open: false });

function MainView() {
  const { snap, progress, refresh } = useSnapshot();
  if (snap?.screen === "daily") return <TitleBar drawer={snap.drawer} attention={agentNeedsAttention(snap)} maximized={snap.maximized} />;
  return <Page snap={snap ?? ({ screen: "starting" } as never)} progress={progress} refresh={refresh} />;
}

/// The drawer's content enters when the backend opens the view and leaves
/// before it hides it (the backend waits for the exit, mod.rs `set_drawer`).
function DrawerView() {
  const { snap, refresh } = useSnapshot();
  const open = !!snap?.drawer;
  useEffect(() => {
    document.documentElement.toggleAttribute("data-open", open);
    if (open) document.querySelector<HTMLElement>(".shell-drawer")?.scrollTo({ top: 0 });
  }, [open]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !document.querySelector("[role=alertdialog]") && closeDrawer();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  return (
    <div className="shell-drawer h-full overflow-y-auto border-l border-stroke-soft-200 bg-bg-page">
      <Settings snap={snap} onClose={closeDrawer} refresh={refresh} />
    </div>
  );
}

document.addEventListener("contextmenu", (e) => e.preventDefault());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <ThemeProvider>
      <LabelsProvider labels={{ close: "关闭", dismiss: "关闭", cancel: "取消" }}>
        {IS_DRAWER ? <DrawerView /> : <MainView />}
      </LabelsProvider>
    </ThemeProvider>
  </StrictMode>,
);
