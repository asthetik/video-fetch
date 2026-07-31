import { useCallback, useState } from "react";
import { AuthStatus } from "./components/AuthStatus";
import { PageShell } from "./components/PageShell";
import { usePageTransition } from "./hooks/usePageTransition";
import type { AppPage } from "./lib/pageTransition";
import { HistoryPage } from "./pages/HistoryPage";
import { HomePage } from "./pages/HomePage";
import { AboutPage } from "./pages/AboutPage";
import { SettingsPage } from "./pages/SettingsPage";
import "./styles.css";

type Page = AppPage;

const NAV_ITEMS: { id: Page; label: string }[] = [
  { id: "home", label: "主页" },
  { id: "history", label: "历史" },
  { id: "settings", label: "设置" },
  { id: "about", label: "关于" },
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
              onClick={() => setPage(item.id)}
              aria-current={page === item.id ? "page" : undefined}
            >
              {item.label}
            </button>
          ))}
        </nav>
        <div className="app-header-end">
          <AuthStatus />
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
            aria-hidden={displayedPage !== "home"}
            {...(displayedPage !== "home" ? { inert: true } : {})}
          >
            <HomePage
              queueRefresh={queueRefresh}
              onQueueRefresh={bumpQueueRefresh}
              onOpenHistory={() => setPage("history")}
            />
          </div>

          {displayedPage === "history" && (
            <HistoryPage onJobsChanged={bumpQueueRefresh} />
          )}
          {displayedPage === "settings" && <SettingsPage />}
          {displayedPage === "about" && <AboutPage />}
        </PageShell>
      </main>
    </div>
  );
}

export default App;
