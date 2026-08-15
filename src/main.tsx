import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { initActivityLog } from "./lib/activityLog";

initActivityLog();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
