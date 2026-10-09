import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";
import { applyTheme, storedTheme } from "./theme";

// Тему — до первой отрисовки: иначе тёмное окно на миг покажется светлым.
applyTheme(storedTheme());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
