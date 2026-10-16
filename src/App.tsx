import { useCallback, useEffect, useState } from "react";
import { House, History, Settings2, FileText, Info } from "lucide-react";
import { motion, MotionConfig } from "motion/react";
import { toast, Toaster } from "sonner";
import { AuthChip } from "./components/AuthChip";
import { PageTransition, type AppPage } from "./components/PageTransition";
import { useTheme } from "./hooks/useTheme";
import { api } from "./lib/tauri";
import { HistoryPage } from "./pages/HistoryPage";
import { HomePage } from "./pages/HomePage";
import { AboutPage } from "./pages/AboutPage";
import { LogsPage } from "./pages/LogsPage";
import { SettingsPage } from "./pages/SettingsPage";
import "./styles.css";

type Page = AppPage;

const NAV_ITEMS: {
  id: Page;
  label: string;
  icon: typeof House;
}[] = [
  { id: "home", label: "主页", icon: House },
  { id: "history", label: "历史", icon: History },
  { id: "settings", label: "设置", icon: Settings2 },
  { id: "logs", label: "日志", icon: FileText },
  { id: "about", label: "关于", icon: Info },
];

// DESIGN.md motion rules: a moving element reads as ease-in-out, and
// micro-interactions stay inside the 90-160ms band, so this sits at the cap.
const NAV_INDICATOR_S = 0.16;

function App() {
  const { resolved } = useTheme();
  const [page, setPage] = useState<Page>("home");
  const [queueRefresh, setQueueRefresh] = useState(0);
  const bumpQueueRefresh = useCallback(() => {
    setQueueRefresh((n) => n + 1);
  }, []);

  // Startup-only notice: when the configured save directory is unavailable
  // the backend redirects this session's downloads to the app-data fallback
  // without touching the setting — say so once instead of letting files
  // quietly land elsewhere.
  useEffect(() => {
    let cancelled = false;
    api
      .getSaveDirNotice()
      .then((notice) => {
        if (!cancelled && notice) {
          toast.warning(notice, {
            description: "本次下载暂存至应用数据目录，设置中的保存路径未更改",
          });
        }
      })
      .catch(() => {
        // A notice failure must never block startup; downloads still run.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Startup-only notice: when cookie materialization failed at launch the
  // login chip keeps reading "logged in" but downloads cannot carry the
  // session — say so once instead of letting the first download fail cold.
  useEffect(() => {
    let cancelled = false;
    api
      .getCookieNotice()
      .then((notice) => {
        if (!cancelled && notice) {
          toast.warning(notice, {
            description: "所有下载入口都会被拒绝并直接报错，不会静默以未登录状态下载",
          });
        }
      })
      .catch(() => {
        // A notice failure must never block startup; downloads still run.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <MotionConfig reducedMotion="user">
      <div className="app">
        <header className="app-header">
          <nav className="app-nav" aria-label="主导航">
            {NAV_ITEMS.map((item) => {
              const active = page === item.id;
              return (
                <button
                  key={item.id}
                  type="button"
                  className={`nav-btn${active ? " active" : ""}`}
                  data-action={`nav-${item.id}`}
                  onClick={() => {
                    setPage(item.id);
                  }}
                  aria-current={active ? "page" : undefined}
                >
                  {active && (
                    <motion.span
                      layoutId="nav-indicator"
                      layoutDependency={page}
                      className="nav-indicator"
                      transition={{ duration: NAV_INDICATOR_S, ease: "easeInOut" }}
                    />
                  )}
                  <item.icon strokeWidth={2} />
                  <span className="nav-btn-label">{item.label}</span>
                </button>
              );
            })}
          </nav>
          <div className="app-header-end">
            <AuthChip />
          </div>
        </header>

        <main className="app-main">
          <PageTransition
            page={page}
            home={(homeActive) => (
              <HomePage
                queueRefresh={queueRefresh}
                onQueueRefresh={bumpQueueRefresh}
                onOpenHistory={() => setPage("history")}
                active={homeActive}
              />
            )}
          >
            {(p) =>
              p === "history" ? (
                <HistoryPage
                  onJobsChanged={bumpQueueRefresh}
                  onGoHome={() => setPage("home")}
                />
              ) : p === "settings" ? (
                <SettingsPage />
              ) : p === "logs" ? (
                <LogsPage />
              ) : (
                <AboutPage />
              )
            }
          </PageTransition>
        </main>

        <Toaster
          position="bottom-right"
          theme={resolved}
          toastOptions={{ className: "app-toast" }}
        />
      </div>
    </MotionConfig>
  );
}

export default App;
