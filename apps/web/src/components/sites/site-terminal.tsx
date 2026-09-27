"use client";

import * as React from "react";
import { useQuery } from "@tanstack/react-query";
import { RefreshCw, TerminalSquare } from "lucide-react";
import type { Site } from "@nsb/schema";
import * as api from "@/lib/api";
import { isTauri, normalizeError, type AppErrorShape } from "@/lib/backend";
import { useT } from "@/lib/store";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { CodeBlock } from "@/components/shared/code-block";

/** 从站点和已安装版本生成只读预览；启动时后端再次校验，客户端不提交命令。 */
export function SiteTerminalButton({ site }: { site: Pick<Site, "id" | "name"> }) {
  const t = useT();
  const [open, setOpen] = React.useState(false);
  const [opening, setOpening] = React.useState(false);
  const running = React.useRef(false);
  const [error, setError] = React.useState<AppErrorShape | null>(null);
  const [opened, setOpened] = React.useState(false);
  const errorRef = React.useRef<HTMLDivElement>(null);
  const environment = useQuery({ queryKey: ["pathenv", "terminal", site.id], queryFn: () => api.terminalEnvironment(site.id), enabled: open,
    staleTime: 0, retry: false, refetchOnWindowFocus: false });
  const problem = error ?? (environment.error ? normalizeError(environment.error) : null);
  React.useEffect(() => { if (problem) errorRef.current?.focus(); }, [problem?.message]);
  const refresh = async () => {
    if (running.current || environment.isFetching) return;
    setError(null); setOpened(false);
    await environment.refetch({ cancelRefetch: false });
  };
  const launch = async () => {
    if (running.current || environment.isFetching || environment.error || !environment.data || !isTauri) return;
    running.current = true; setOpening(true); setError(null); setOpened(false);
    try { await api.openTerminal(environment.data.revision, site.id); setOpened(true); }
    catch (e) { setError(normalizeError(e)); }
    finally { running.current = false; setOpening(false); }
  };
  const data = environment.data;
  return <>
    <Tooltip><TooltipTrigger asChild><Button variant="ghost" size="icon-sm" className="text-faint hover:text-foreground" aria-label={t("dashboard.openTerminal")}
      onClick={() => { setError(null); setOpened(false); setOpen(true); }}><TerminalSquare className="h-3.5 w-3.5" /></Button></TooltipTrigger><TooltipContent>{t("sites.terminal.title")}</TooltipContent></Tooltip>
    <Dialog open={open} onOpenChange={(next) => { if (!running.current) setOpen(next); }}>
      <DialogContent hideClose={opening} className="flex max-h-[85dvh] max-w-xl flex-col overflow-hidden">
        <DialogHeader className="shrink-0 pr-5"><DialogTitle title={site.name} className="line-clamp-2 leading-snug [overflow-wrap:anywhere]">{t("sites.terminal.title")} · {site.name}</DialogTitle><DialogDescription>{t("sites.terminal.hint")}</DialogDescription></DialogHeader>
        <div className="min-h-0 min-w-0 space-y-4 overflow-y-auto text-xs leading-relaxed" aria-busy={environment.isFetching}>
          {!isTauri && <p className="text-warn">{t("sites.terminal.demo")}</p>}
          {environment.isFetching && <p role="status" className="text-secondary">{t("common.loading")}</p>}
          {problem && <div ref={errorRef} tabIndex={-1} role="alert" className="space-y-2 rounded-lg bg-error-soft p-3 text-error outline-none focus-visible:ring-2 focus-visible:ring-error [overflow-wrap:anywhere]"><p>{problem.message}</p>{problem.hint && <p>{problem.hint}</p>}</div>}
          {data && !environment.error && !environment.isFetching && <>
            <div className="space-y-1"><p className="text-secondary">{t("sites.terminal.directory")}</p><p className="font-mono [overflow-wrap:anywhere]">{data.cwd}</p></div>
            {data.warnings.length > 0 && <div role="status" className="rounded-lg bg-warn-soft p-3 text-warn"><p>{t("tools.termInjectSkipped")}</p><ul className="mt-2 list-disc space-y-1 pl-4 [overflow-wrap:anywhere]">{data.warnings.map((warning, i) => <li key={i}>{warning}</li>)}</ul></div>}
            <div className="rounded-lg bg-fill px-3">{data.entries.map((entry) => <div key={entry.id} className="space-y-1 border-b border-dashed border-separator py-3 last:border-0"><p className="flex flex-wrap items-center justify-between gap-2"><span className="min-w-0 font-medium [overflow-wrap:anywhere]">{entry.label}</span><code className="min-w-0 [overflow-wrap:anywhere]">{entry.version}</code></p><p className="font-mono text-secondary [overflow-wrap:anywhere]">{entry.binDir}</p></div>)}
              {data.entries.length === 0 && <p className="py-3 text-secondary">{t("sites.terminal.empty")}</p>}</div>
            <p className="text-secondary">{t("sites.terminal.selectionHint")}</p>
            <p className="text-secondary">{t("sites.terminal.scope")}</p>
            {data.script && <details><summary className="cursor-pointer text-secondary">{t("sites.terminal.script")}</summary><div className="mt-3"><CodeBlock code={data.script} lang="shell" title={data.shell === "powershell" ? "PowerShell" : "Bash / Zsh"} maxHeight={200} compact /></div></details>}
          </>}
          {opened && <p role="status" className="rounded-lg bg-fill p-3 text-secondary">{t("sites.terminal.opened")}</p>}
        </div>
        <DialogFooter className="shrink-0 flex-wrap gap-2"><Button variant="ghost" disabled={opening || environment.isFetching} onClick={() => void refresh()}><RefreshCw className="h-3.5 w-3.5" />{t("tools.refresh")}</Button><Button disabled={opening || environment.isFetching || !!environment.error || error?.code === "TERMINAL_ENV_CHANGED" || !data || !isTauri} onClick={() => void launch()}>{t(opening ? "sites.terminal.opening" : "dashboard.openTerminal")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  </>;
}
