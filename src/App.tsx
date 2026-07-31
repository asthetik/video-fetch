import { useCallback, useState } from "react";
import { AuthStatus } from "./components/AuthStatus";
import { HistoryPage } from "./pages/HistoryPage";
import { HomePage } from "./pages/HomePage";
import { AboutPage } from "./pages/AboutPage";
import { SettingsPage } from "./pages/SettingsPage";
import "./styles.css";

type Page = "home" | "history" | "settings" | "about";

const NAV_ITEMS: { id: Page; label: string }[] = [
  { id: "home", label: "主页" },
  { id: "history", label: "历史" },
  { id: "settings", label: "设置" },
  { id: "about", label: "关于" },
];

function App() {
  const [page, setPage] = useState<Page>("home");
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
        {/* Keep home mounted so pasted URL and resolved video survive tab switches. */}
        <div
          className={page === "home" ? undefined : "page-hidden"}
          aria-hidden={page !== "home"}
        >
          <HomePage
            queueRefresh={queueRefresh}
            onQueueRefresh={bumpQueueRefresh}
            onOpenHistory={() => setPage("history")}
          />
        </div>
        {page === "history" && (
          <HistoryPage onJobsChanged={bumpQueueRefresh} />
        )}
        {page === "settings" && <SettingsPage />}
        {page === "about" && <AboutPage />}
      </main>
    </div>
  );
}

export default App;
