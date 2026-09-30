import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../index.css";
import { WorkerStatusApp } from "./page";

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <WorkerStatusApp />
    </StrictMode>,
  );
}
