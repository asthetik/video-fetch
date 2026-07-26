import { useState } from "react";
import { AuthStatus } from "./components/AuthStatus";
import { HomePage } from "./pages/HomePage";
import "./styles.css";

type Page = "home" | "history" | "settings" | "about";

const NAV_ITEMS: { id: Page; label: string }[] = [
  { id: "home", label: "主页" },
  { id: "history", label: "历史" },
  { id: "settings", label: "设置" },
  { id: "about", label: "关于" },
];

export default function App() {
  const [page, setPage] = useState<Page>("home");

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
        <div
          className={page === "home" ? undefined : "page-hidden"}
          aria-hidden={page !== "home"}
        >
          <HomePage />
        </div>
        {page === "history" && <p className="page-placeholder">历史（即将接入）</p>}
        {page === "settings" && <p className="page-placeholder">设置（即将接入）</p>}
        {page === "about" && <p className="page-placeholder">关于（即将接入）</p>}
      </main>
    </div>
  );
}
