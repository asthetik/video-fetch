import { useCallback, useState } from "react";
import { House, History, Settings2, FileText, Info } from "lucide-react";
import { AuthChip } from "./components/AuthChip";
import { PageShell } from "./components/PageShell";
import { usePageTransition } from "./hooks/usePageTransition";
import type { AppPage } from "./lib/pageTransition";
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
  const [page, setPage] = useState<Page>("home");
  const {
    displayedPage,
    phase,
    reducedMotion,
    onExitComplete,
    onEnterComplete,
  } = usePageTransition(page);
  const [queueRefresh, setQueueRefresh] = useState(0);
  const bumpQueueRefresh = useCallback(() => {
    setQueueRefresh((n) => n + 1);
  }, []);

  return (
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
        <PageShell
          phase={phase}
          reducedMotion={reducedMotion}
          onExitComplete={onExitComplete}
          onEnterComplete={onEnterComplete}
        >
          {/* Keep home mounted so pasted URL and resolved video survive tab switches. */}
          <div
            className={displayedPage === "home" ? undefined : "page-hidden"}
            aria-hidden={displayedPage !== "home" ? true : undefined}
            {...(displayedPage !== "home" ? { inert: true } : {})}
          >
            <HomePage
              queueRefresh={queueRefresh}
              onQueueRefresh={bumpQueueRefresh}
              onOpenHistory={() => setPage("history")}
              active={displayedPage === "home"}
            />
          </div>

          {displayedPage === "history" && (
            <HistoryPage onJobsChanged={bumpQueueRefresh} />
          )}
          {displayedPage === "settings" && <SettingsPage />}
          {displayedPage === "logs" && <LogsPage />}
          {displayedPage === "about" && <AboutPage />}
        </PageShell>
      </main>
    </div>
  );
}

export default App;
