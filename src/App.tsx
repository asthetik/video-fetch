import { useCallback, useState } from "react";
import { House, History, Settings2, FileText, Info } from "lucide-react";
import { MotionConfig } from "motion/react";
import { Toaster } from "sonner";
import { AuthChip } from "./components/AuthChip";
import { PageTransition, type AppPage } from "./components/PageTransition";
import { useTheme } from "./hooks/useTheme";
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

function App() {
  const { resolved } = useTheme();
  const [page, setPage] = useState<Page>("home");
  const [queueRefresh, setQueueRefresh] = useState(0);
  const bumpQueueRefresh = useCallback(() => {
    setQueueRefresh((n) => n + 1);
  }, []);

  return (
    <MotionConfig reducedMotion="user">
      <div className="app">
        <header className="app-header">
          <nav className="app-nav" aria-label="主导航">
            {NAV_ITEMS.map((item) => (
              <button
                key={item.id}
                type="button"
                className={`nav-btn${page === item.id ? " active" : ""}`}
                data-action={`nav-${item.id}`}
                onClick={() => {
                  setPage(item.id);
                }}
                aria-current={page === item.id ? "page" : undefined}
              >
                <item.icon strokeWidth={2} />
                {item.label}
              </button>
            ))}
          </nav>
          <div className="app-header-end">
            <AuthChip onOpenSettings={() => setPage("settings")} />
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
