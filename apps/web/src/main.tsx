import * as React from "react";
import { createRoot } from "react-dom/client";
import { HashRouter, Navigate, Route, Routes, useLocation } from "react-router-dom";
import RootLayout from "./app/layout";
import HomePage from "./app/page";
import { useT } from "./lib/store";
import { Button } from "./components/ui/button";

// Keep page chunks separate: opening the dashboard must not load every editor/workspace.
const pages = [
  ["backups", React.lazy(() => import("./app/backups/page"))],
  ["configuration", React.lazy(() => import("./app/configuration/page"))],
  ["databases", React.lazy(() => import("./app/databases/page"))],
  ["diagnostics", React.lazy(() => import("./app/diagnostics/page"))],
  ["environment", React.lazy(() => import("./app/environment/page"))],
  ["logs", React.lazy(() => import("./app/logs/page"))],
  ["models", React.lazy(() => import("./app/models/page"))],
  ["network", React.lazy(() => import("./app/network/page"))],
  ["packages", React.lazy(() => import("./app/packages/page"))],
  ["proxy", React.lazy(() => import("./app/proxy/page"))],
  ["rewrites", React.lazy(() => import("./app/rewrites/page"))],
  ["settings", React.lazy(() => import("./app/settings/page"))],
  ["sites", React.lazy(() => import("./app/sites/page"))],
  ["stacks", React.lazy(() => import("./app/stacks/page"))],
  ["tasks", React.lazy(() => import("./app/tasks/page"))],
  ["tls", React.lazy(() => import("./app/tls/page"))],
  ["tools", React.lazy(() => import("./app/tools/page"))],
  ["tunnels", React.lazy(() => import("./app/tunnels/page"))],
] as const;

class PageBoundary extends React.Component<React.PropsWithChildren<{ fallback: React.ReactNode }>, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() { return this.state.failed ? this.props.fallback : this.props.children; }
}

function PageRoutes() {
  const { pathname } = useLocation();
  const t = useT();
  return (
    <PageBoundary key={pathname} fallback={
      <div role="alert" className="space-y-3 rounded-xl border border-border bg-card p-5">
        <p>{t("app.pageFailed")}</p>
        <Button variant="secondary" onClick={() => window.location.reload()}>{t("app.reload")}</Button>
      </div>
    }>
      <React.Suspense fallback={<p role="status" className="py-8 text-center text-muted">{t("common.loading")}</p>}>
        <RouteScroll />
        <Routes>
          <Route path="/" element={<HomePage />} />
          {pages.map(([path, Page]) => <Route key={path} path={`/${path}`} element={<Page />} />)}
          <Route path="*" element={<Navigate to="/" replace />} />
        </Routes>
      </React.Suspense>
    </PageBoundary>
  );
}

function RouteScroll() {
  const { pathname, hash } = useLocation();
  React.useEffect(() => {
    document.querySelector("main")?.scrollTo(0, 0);
    if (hash) {
      let id = hash.slice(1);
      try { id = decodeURIComponent(id); } catch { /* 保留无法解码的原始锚点。 */ }
      document.getElementById(id)?.scrollIntoView({ block: "start" });
    }
  }, [pathname, hash]);
  return null;
}

createRoot(document.getElementById("root")!).render(
  <HashRouter>
    <RootLayout><PageRoutes /></RootLayout>
  </HashRouter>,
);
